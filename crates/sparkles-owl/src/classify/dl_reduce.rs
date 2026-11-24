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

use super::Excluded;
use super::dl;
use super::index::Gap;
use crate::model::*;
use crate::vocab;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeSet;

/// The IRI prefix of the names that the reductions make up.
pub const PREFIX: &str = "urn:sparkles:internal:";

/// The most axioms that the keys' reductions make, whatever the memory budget.
const KEY_AXIOMS: usize = 1 << 17;

/// The bytes of memory budget that one reduction axiom may take.
const AXIOM_BYTES: usize = 2048;

/// The axioms of a rewritten ontology.
pub struct Reduction {
    /// the axioms that the rewriting makes, each standing for the original axiom at the
    /// same place of `origin`
    pub axioms: Vec<AxiomKind>,
    pub origin: Vec<usize>,
    /// the original axioms that the rewriting replaces or drops
    pub replaced: Vec<bool>,
}

impl Reduction {
    /// The axioms of the rewritten ontology.
    pub fn all<'a>(&'a self, axioms: &[&'a AxiomKind]) -> Vec<&'a AxiomKind> {
        let mut all: Vec<&AxiomKind> = axioms
            .iter()
            .zip(&self.replaced)
            .filter(|(_, r)| !**r)
            .map(|(k, _)| *k)
            .collect();
        all.extend(self.axioms.iter());
        all
    }

    /// Puts the original axioms in place of the rewritten ones that the run's report
    /// holds. A rewritten axiom stands for every original axiom that made it, and for
    /// itself as well when the ontology states it unchanged, so one entry of the report
    /// can become several. The lookup holds only the report's axioms, so it costs memory
    /// in proportion to the report and not to the rewriting.
    pub fn restore(&self, axioms: &[&AxiomKind], excluded: &mut Vec<Excluded>) {
        if excluded.is_empty() {
            return;
        }
        // the origins of each axiom of the report, and whether the ontology states it as
        // it is
        let mut origin: FxHashMap<AxiomKind, (Vec<usize>, bool)> = excluded
            .iter()
            .map(|e| (e.axiom.clone(), (Vec::new(), false)))
            .collect();
        for (k, &i) in self.axioms.iter().zip(&self.origin) {
            if let Some((from, _)) = origin.get_mut(k)
                && from.last() != Some(&i)
            {
                from.push(i);
            }
        }
        for (k, r) in axioms.iter().zip(&self.replaced) {
            if !*r && let Some((_, stated)) = origin.get_mut(*k) {
                *stated = true;
            }
        }
        let mut out = Vec::with_capacity(excluded.len());
        // an original axiom whose rewritten axioms are left out for several reasons of
        // one kind is reported once for that kind, by the first of them
        let mut told: FxHashSet<(usize, std::mem::Discriminant<Gap>)> = FxHashSet::default();
        excluded.sort();
        for e in excluded.drain(..) {
            match origin.get(&e.axiom) {
                Some((from, stated)) if !from.is_empty() => {
                    let kind = std::mem::discriminant(&e.gap);
                    out.extend(from.iter().filter(|&&i| told.insert((i, kind))).map(|&i| {
                        Excluded {
                            axiom: axioms[i].clone(),
                            gap: e.gap.clone(),
                        }
                    }));
                    if *stated {
                        out.push(e);
                    }
                }
                _ => out.push(e),
            }
        }
        out.sort();
        out.dedup();
        *excluded = out;
    }

    /// What the original axioms outside OWL 2 EL need, for the run's statistics, with the
    /// report of the axioms the run left out.
    /// `chains` are the original chains outside OWL 2 EL for their ranges alone, which are
    /// Horn, as the run without the rewriting counts them.
    pub fn outside_el(
        &self,
        axioms: &[&AxiomKind],
        chains: &[&AxiomKind],
        excluded: &[Excluded],
    ) -> dl::OutsideEl {
        let mut out = dl::OutsideEl::default();
        let mut seen: BTreeSet<&AxiomKind> = BTreeSet::new();
        for k in axioms {
            if super::logical(k) && super::el_break(k).is_some() && seen.insert(k) {
                out.add(dl::shape(k));
            }
        }
        for k in chains {
            if seen.insert(k) {
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
    ObjectProperty::new(format!("{PREFIX}u"))
}

fn hub() -> Individual {
    Individual::named(format!("{PREFIX}hub"))
}

fn ce_has_top(c: &ClassExpression) -> bool {
    use ClassExpression as C;
    match c {
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => cs.iter().any(ce_has_top),
        C::ObjectComplementOf(f) => ce_has_top(f),
        C::ObjectSomeValuesFrom(p, f) | C::ObjectAllValuesFrom(p, f) => is_top(p) || ce_has_top(f),
        C::ObjectHasValue(p, _) | C::ObjectHasSelf(p) => is_top(p),
        C::ObjectMinCardinality(_, p, f)
        | C::ObjectMaxCardinality(_, p, f)
        | C::ObjectExactCardinality(_, p, f) => is_top(p) || f.as_deref().is_some_and(ce_has_top),
        _ => false,
    }
}

fn ax_has_top(k: &AxiomKind) -> bool {
    use AxiomKind as K;
    match k {
        K::SubClassOf(a, b) => ce_has_top(a) || ce_has_top(b),
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => {
            cs.iter().any(ce_has_top)
        }
        K::ClassAssertion(c, _) => ce_has_top(c),
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => is_top(p) || ce_has_top(c),
        K::SubObjectPropertyOf(s, p) => {
            is_top(p)
                || match s {
                    SubObjectPropertyExpression::Expression(q) => is_top(q),
                    SubObjectPropertyExpression::Chain(qs) => qs.iter().any(is_top),
                }
        }
        K::EquivalentObjectProperties(ps) | K::DisjointObjectProperties(ps) => {
            ps.iter().any(is_top)
        }
        K::InverseObjectProperties(p, q) => is_top(p) || is_top(q),
        K::FunctionalObjectProperty(p)
        | K::InverseFunctionalObjectProperty(p)
        | K::ReflexiveObjectProperty(p)
        | K::IrreflexiveObjectProperty(p)
        | K::SymmetricObjectProperty(p)
        | K::AsymmetricObjectProperty(p)
        | K::TransitiveObjectProperty(p) => is_top(p),
        K::ObjectPropertyAssertion(p, ..) | K::NegativeObjectPropertyAssertion(p, ..) => is_top(p),
        K::HasKey(c, ops, _) => ce_has_top(c) || ops.iter().any(is_top),
        _ => false,
    }
}

/// What the rewriting makes of one axiom.
enum Rt {
    Drop,
    New(Vec<AxiomKind>),
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

    fn ces(&mut self, cs: &BTreeSet<ClassExpression>) -> Result<BTreeSet<ClassExpression>, ()> {
        cs.iter().map(|c| self.ce(c)).collect()
    }

    /// The images of the members of a disjointness, and an axiom `D ⊑ ⊥` for each image
    /// `D` of more than one member. Two members with one image are equivalent, so their
    /// disjointness makes the image empty, which the set of images alone would lose.
    fn disjoint(
        &mut self,
        cs: &BTreeSet<ClassExpression>,
    ) -> Result<(BTreeSet<ClassExpression>, Vec<AxiomKind>), ()> {
        let mut images = BTreeSet::new();
        let mut empty = BTreeSet::new();
        for c in cs {
            let d = self.ce(c)?;
            if images.contains(&d) {
                empty.insert(d);
            } else {
                images.insert(d);
            }
        }
        let empty = empty
            .into_iter()
            .map(|d| AxiomKind::SubClassOf(d, ClassExpression::Class(Class::nothing())))
            .collect();
        Ok((images, empty))
    }

    fn ce(&mut self, c: &ClassExpression) -> Result<ClassExpression, ()> {
        use ClassExpression as C;
        let filler = |s: &mut Cx, f: &Option<Box<ClassExpression>>| match f {
            Some(f) => s.ce(f).map(|f| Some(Box::new(f))),
            None => Ok(None),
        };
        Ok(match c {
            C::ObjectIntersectionOf(cs) => C::ObjectIntersectionOf(self.ces(cs)?),
            C::ObjectUnionOf(cs) => C::ObjectUnionOf(self.ces(cs)?),
            C::ObjectComplementOf(f) => C::ObjectComplementOf(Box::new(self.ce(f)?)),
            C::ObjectSomeValuesFrom(p, f) => {
                C::ObjectSomeValuesFrom(self.ope(p), Box::new(self.ce(f)?))
            }
            C::ObjectAllValuesFrom(p, f) => {
                C::ObjectAllValuesFrom(self.ope(p), Box::new(self.ce(f)?))
            }
            C::ObjectHasValue(p, i) => C::ObjectHasValue(self.ope(p), i.clone()),
            C::ObjectHasSelf(p) if is_top(p) => return Err(()),
            // at least none is every element, and at least one is an existential
            C::ObjectMinCardinality(0, p, _) if is_top(p) => C::Class(Class::thing()),
            C::ObjectMinCardinality(1, p, f) if is_top(p) => C::ObjectSomeValuesFrom(
                self.ope(p),
                Box::new(match f {
                    Some(f) => self.ce(f)?,
                    None => C::Class(Class::thing()),
                }),
            ),
            C::ObjectMinCardinality(_, p, _) if is_top(p) => return Err(()),
            C::ObjectMinCardinality(n, p, f) => {
                C::ObjectMinCardinality(*n, p.clone(), filler(self, f)?)
            }
            C::ObjectMaxCardinality(_, p, _) | C::ObjectExactCardinality(_, p, _) if is_top(p) => {
                return Err(());
            }
            C::ObjectMaxCardinality(n, p, f) => {
                C::ObjectMaxCardinality(*n, p.clone(), filler(self, f)?)
            }
            C::ObjectExactCardinality(n, p, f) => {
                C::ObjectExactCardinality(*n, p.clone(), filler(self, f)?)
            }
            _ => c.clone(),
        })
    }

    fn retarget(&mut self, k: &AxiomKind) -> Result<Rt, ()> {
        use AxiomKind as K;
        let sub = |p: ObjectPropertyExpression, q: &ObjectPropertyExpression| {
            K::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(p), q.clone())
        };
        Ok(Rt::New(vec![match k {
            K::SubClassOf(a, b) => K::SubClassOf(self.ce(a)?, self.ce(b)?),
            K::EquivalentClasses(cs) => K::EquivalentClasses(self.ces(cs)?),
            K::DisjointClasses(cs) => {
                let (images, mut out) = self.disjoint(cs)?;
                if images.len() >= 2 {
                    out.push(K::DisjointClasses(images));
                }
                return Ok(Rt::New(out));
            }
            // the union keeps the set of images, and the disjointness of its members
            // follows the rule of `DisjointClasses`
            K::DisjointUnion(c, cs) => {
                let (images, mut out) = self.disjoint(cs)?;
                if images.len() >= 2 {
                    out.push(K::DisjointUnion(c.clone(), images));
                } else if let Some(d) = images.into_iter().next() {
                    let c = ClassExpression::Class(c.clone());
                    if c != d {
                        out.push(K::EquivalentClasses([c, d].into()));
                    }
                }
                return Ok(Rt::New(out));
            }
            K::ClassAssertion(c, i) => K::ClassAssertion(self.ce(c)?, i.clone()),
            // every element is in the domain and in the range of the universal property
            K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) if is_top(p) => {
                K::SubClassOf(ClassExpression::Class(Class::thing()), self.ce(c)?)
            }
            K::ObjectPropertyDomain(p, c) => K::ObjectPropertyDomain(p.clone(), self.ce(c)?),
            K::ObjectPropertyRange(p, c) => K::ObjectPropertyRange(p.clone(), self.ce(c)?),
            K::SubObjectPropertyOf(_, sup) if is_top(sup) => return Ok(Rt::Drop),
            K::SubObjectPropertyOf(s, sup) => K::SubObjectPropertyOf(
                match s {
                    SubObjectPropertyExpression::Expression(p) => {
                        SubObjectPropertyExpression::Expression(self.ope(p))
                    }
                    SubObjectPropertyExpression::Chain(ps) => {
                        SubObjectPropertyExpression::Chain(ps.iter().map(|p| self.ope(p)).collect())
                    }
                },
                sup.clone(),
            ),
            // the properties equivalent to the universal property are below it
            K::EquivalentObjectProperties(ps) => {
                let rest: Vec<&ObjectPropertyExpression> =
                    ps.iter().filter(|p| !is_top(p)).collect();
                if rest.is_empty() {
                    return Ok(Rt::Drop);
                }
                return Ok(Rt::New(
                    rest.into_iter()
                        .map(|p| {
                            let top = self
                                .ope(&ObjectPropertyExpression::Property(ObjectProperty::top()));
                            sub(top, p)
                        })
                        .collect(),
                ));
            }
            K::InverseObjectProperties(p, q) => match (is_top(p), is_top(q)) {
                (true, true) => return Ok(Rt::Drop),
                (true, false) | (false, true) => {
                    let other = if is_top(p) { q } else { p };
                    let top = self.ope(&ObjectPropertyExpression::Property(ObjectProperty::top()));
                    sub(top, other)
                }
                _ => return Err(()),
            },
            K::SymmetricObjectProperty(p)
            | K::TransitiveObjectProperty(p)
            | K::ReflexiveObjectProperty(p)
                if is_top(p) =>
            {
                return Ok(Rt::Drop);
            }
            K::ObjectPropertyAssertion(p, ..) if is_top(p) => return Ok(Rt::Drop),
            K::NegativeObjectPropertyAssertion(p, a, b) => {
                K::NegativeObjectPropertyAssertion(self.ope(p), a.clone(), b.clone())
            }
            _ => return Err(()),
        }]))
    }
}

/// Whether the axioms hold something that keeps a rooted run of Tier D from them whatever
/// the rewriting does, data, the bottom property or a reflexive property, or a name with
/// the prefix of the made-up names.
fn blocked(k: &AxiomKind, inds: &mut BTreeSet<NamedIndividual>) -> bool {
    let mut s = Signature::default();
    s.axiom(k);
    let mut bad = matches!(k, AxiomKind::ReflexiveObjectProperty(p) if !is_top(p));
    for e in &s.0 {
        // a name of the ontology's own that looks like a made-up one would meet them
        if e.iri().as_str().starts_with(PREFIX) {
            bad = true;
        }
        match e {
            Entity::DataProperty(_) | Entity::Datatype(_) => bad = true,
            Entity::ObjectProperty(p) if p.as_str() == vocab::OWL_BOTTOM_OBJECT_PROPERTY => {
                bad = true
            }
            Entity::NamedIndividual(i) => {
                inds.insert(i.clone());
            }
            _ => {}
        }
    }
    bad
}

/// The rewriting of the axioms for a rooted run, or `None` when no axiom needs it, when
/// the ontology has no axiom that needs case analysis, which the Horn rules take, when
/// something keeps Tier D from it anyway, or when an axiom has no reduction or the keys'
/// reductions pass the bound. `memory` is the run's memory budget.
pub fn plan(axioms: &[&AxiomKind], memory: Option<usize>) -> Option<Reduction> {
    // an inclusion in the top property alone needs no rewriting, since the run takes it
    // as an axiom that adds nothing (roadmap item 1282)
    let wants = |k: &AxiomKind| {
        ax_has_top(k) && !super::index::top_inclusion(k)
            || matches!(k, AxiomKind::HasKey(_, ops, dps) if !ops.is_empty() && dps.is_empty())
    };
    if !axioms.iter().any(|k| wants(k)) {
        return None;
    }
    let mut inds = BTreeSet::new();
    for k in axioms {
        // an inclusion in a top property is dropped below, and its subproperty, such as
        // a data property, keeps nothing from the rewriting
        if super::index::top_inclusion(k) {
            continue;
        }
        if blocked(k, &mut inds) {
            return None;
        }
        if matches!(k, AxiomKind::HasKey(_, _, dps) if !dps.is_empty()) {
            return None;
        }
    }
    if !axioms.iter().any(|k| !dl::shape(k).horn) {
        return None;
    }
    let inds: Vec<NamedIndividual> = inds.into_iter().collect();
    let n = inds.len();
    let pairs = n * n.saturating_sub(1) / 2;
    let limit = memory.map_or(KEY_AXIOMS, |m| (m / AXIOM_BYTES).min(KEY_AXIOMS));
    let mut cx = Cx::default();
    let mut out = Reduction {
        axioms: Vec::new(),
        origin: Vec::new(),
        replaced: vec![false; axioms.len()],
    };
    let mut first_use = None;
    let mut keys = 0usize;
    let mut total = 0usize;
    for (i, &k) in axioms.iter().enumerate() {
        if let AxiomKind::HasKey(c, ops, _) = k
            && !ops.is_empty()
        {
            let m = ops.len();
            total = total.saturating_add(m.saturating_mul(n).saturating_mul(pairs + 1) + pairs);
            if total > limit {
                return None;
            }
            let before = cx.used;
            let c = cx.ce(c).ok()?;
            let rs: Vec<ObjectPropertyExpression> = ops.iter().map(|p| cx.ope(p)).collect();
            if cx.used && !before {
                first_use.get_or_insert(i);
            }
            // a class expression other than a name is named once, so that the many
            // axioms of the key hold a name and not a copy of the expression
            let c = match c {
                ClassExpression::Class(_) => c,
                c => {
                    let name = ClassExpression::Class(Class::new(format!("{PREFIX}k{keys}")));
                    out.axioms
                        .push(AxiomKind::EquivalentClasses([name.clone(), c].into()));
                    out.origin.push(i);
                    name
                }
            };
            let made = key(keys, &c, &rs, &inds);
            keys += 1;
            out.origin.extend(std::iter::repeat_n(i, made.len()));
            out.axioms.extend(made);
            out.replaced[i] = true;
            continue;
        }
        if !ax_has_top(k) {
            continue;
        }
        let before = cx.used;
        match cx.retarget(k).ok()? {
            Rt::Drop => {}
            Rt::New(v) => {
                out.origin.extend(std::iter::repeat_n(i, v.len()));
                out.axioms.extend(v);
            }
        }
        if cx.used && !before {
            first_use.get_or_insert(i);
        }
        out.replaced[i] = true;
    }
    if let Some(i) = first_use {
        let up = ObjectPropertyExpression::Property(u());
        let thing = ClassExpression::Class(Class::thing());
        for k in [
            AxiomKind::SubClassOf(
                thing,
                ClassExpression::ObjectSomeValuesFrom(
                    up.clone(),
                    Box::new(ClassExpression::ObjectOneOf([hub()].into())),
                ),
            ),
            AxiomKind::SymmetricObjectProperty(up.clone()),
            AxiomKind::TransitiveObjectProperty(up),
        ] {
            out.axioms.push(k);
            out.origin.push(i);
        }
    }
    // the classes and individuals of the replaced axioms stay in the signature, though the
    // rewriting may drop them
    let mut sig = Signature::default();
    let mut at: FxHashMap<Entity, usize> = FxHashMap::default();
    for (i, k) in axioms.iter().enumerate() {
        if out.replaced[i] {
            let mut one = Signature::default();
            one.axiom(k);
            for e in one.0 {
                at.entry(e.clone()).or_insert(i);
                sig.0.insert(e);
            }
        }
    }
    for e in sig.0 {
        let k = match &e {
            Entity::Class(c) => AxiomKind::Declaration(Entity::Class(c.clone())),
            Entity::NamedIndividual(n) => AxiomKind::ClassAssertion(
                ClassExpression::Class(Class::thing()),
                Individual::Named(n.clone()),
            ),
            _ => continue,
        };
        out.axioms.push(k);
        out.origin.push(at[&e]);
    }
    out.replaced.iter().any(|r| *r).then_some(out)
}

/// The class axioms that stand for the key `HasKey(c (rs) ())`, number `k`, over the
/// named individuals `inds`.
fn key(
    k: usize,
    c: &ClassExpression,
    rs: &[ObjectPropertyExpression],
    inds: &[NamedIndividual],
) -> Vec<AxiomKind> {
    use ClassExpression as C;
    let one = |i: &NamedIndividual| C::ObjectOneOf([Individual::Named(i.clone())].into());
    let mut out = Vec::new();
    if inds.len() < 2 {
        return out;
    }
    // V_jb holds the r_j-values of b when b is a C
    let v = |j: usize, b: usize| Class::new(format!("{PREFIX}v{k}-{j}-{b}"));
    for (b, ib) in inds.iter().enumerate().skip(1) {
        for (j, r) in rs.iter().enumerate() {
            out.push(AxiomKind::SubClassOf(
                C::ObjectIntersectionOf([one(ib), c.clone()].into()),
                C::ObjectAllValuesFrom(r.clone(), Box::new(C::Class(v(j, b)))),
            ));
        }
    }
    for (a, ia) in inds.iter().enumerate() {
        for (b, ib) in inds.iter().enumerate().skip(a + 1) {
            let mut body = vec![one(ia), c.clone()];
            for (j, r) in rs.iter().enumerate() {
                // M_jab: a has a named r_j-value that b, a C, has too
                let m = Class::new(format!("{PREFIX}m{k}-{j}-{a}-{b}"));
                for ic in inds {
                    out.push(AxiomKind::SubClassOf(
                        C::ObjectIntersectionOf(
                            [
                                one(ia),
                                C::ObjectSomeValuesFrom(
                                    r.clone(),
                                    Box::new(C::ObjectIntersectionOf(
                                        [one(ic), C::Class(v(j, b))].into(),
                                    )),
                                ),
                            ]
                            .into(),
                        ),
                        C::Class(m.clone()),
                    ));
                }
                body.push(C::Class(m));
            }
            out.push(AxiomKind::SubClassOf(
                C::ObjectIntersectionOf(body.into_iter().collect()),
                one(ib),
            ));
        }
    }
    out
}
