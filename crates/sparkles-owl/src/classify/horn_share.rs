//! Whether a rooted Tier D run without counting gives its axioms to the Horn rules
//! (roadmap item 1286).
//!
//! A rooted run that was not asked to count leaves out a Horn axiom that needs counting,
//! such as a functional property or a minimum above one, while a run without
//! [`Limits::rooted`](super::Limits::rooted) gives the same ontology to the Horn rules,
//! which take it. Such a run refuses Tier D before it normalizes anything, so that the
//! Horn rules take the axioms in the same build, with the interner as a run without the
//! option leaves it.
//!
//! The check reads the axioms, the interner and the EL axioms and changes none of them.
//! Its walks allocate nothing, and it polls `stop` every [`POLL_TICKS`] steps. It refuses
//! only for an axiom that the Tier D normalization without counting leaves out for a
//! construct that needs counting and for no other reason, and that the Horn normalization
//! takes. Each half mirrors its normalization structurally, with the bound on the minimums
//! that each one unfolds and the repeats that the Horn normalization removes from a
//! disjunction once it has interned its disjuncts. Where the mirror cannot tell, as with a
//! role whose simplicity its bounded search does not settle or two disjuncts whose
//! normalized forms it does not compare, it answers that the axiom does not refuse, which
//! keeps the rooted run's answer as it was before the item.

use super::dl_norm::MAX_MIN;
use super::horn::MAX_AT_LEAST;
use super::index::{Axioms, Halt, Interner, Role, RoleName};
use crate::model::*;
use crate::vocab;

use AxiomKind as K;
use ClassExpression as C;
use ObjectPropertyExpression as P;

/// The steps between two polls of `stop`.
const POLL_TICKS: usize = 128;
/// The nesting that both normalizations refuse past.
const MAX_DEPTH: usize = 256;
/// The steps of an inclusion's flattening that both normalizations refuse past.
const MAX_STEPS: usize = 4096;
/// How deep the search for a chain below a role goes before it calls the role not simple.
const ROLE_DEPTH: usize = 64;
/// How many role inclusions the search for a chain below one role reads at most before it
/// calls the role not simple.
const ROLE_WORK: usize = 1 << 22;
/// How many comparisons the test of two disjuncts for one normalized expression makes at
/// most before it calls them distinct.
const SAME_WORK: usize = 4096;

/// The role box that the Horn rules would build for the check.
pub enum Roles<'a> {
    /// a run over the original axioms: the role axioms of its EL part and those among the
    /// candidates
    El(&'a Axioms),
    /// a run over item 805's rewriting: the Horn rules take the original axioms that each
    /// rewritten one stands for, with the role box of the original axioms, since a refused
    /// run falls back to them
    Original(&'a super::dl_reduce::Reduction, &'a [&'a AxiomKind]),
}

/// Whether the Tier D normalization of a rooted run without counting refuses the axioms
/// outside OWL 2 EL so that the Horn rules take them.
///
/// Over item 805's rewriting, the Tier D half reads the rewritten axioms, which are what
/// the rooted rules see, and the Horn half reads the original axioms that each one stands
/// for. A rewritten Horn piece of a disjunctive axiom does not refuse, since the Horn rules
/// do not take the disjunctive axiom, and the rewriting is kept.
pub fn refuses(
    candidates: &[&AxiomKind],
    it: &Interner,
    roles: Roles<'_>,
    stop: &dyn Fn() -> bool,
) -> Result<bool, Halt> {
    Ok(refusing(candidates, it, roles, stop)?.is_some())
}

/// The candidate that [`refuses`] refuses for, the first one.
fn refusing(
    candidates: &[&AxiomKind],
    it: &Interner,
    roles: Roles<'_>,
    stop: &dyn Fn() -> bool,
) -> Result<Option<usize>, Halt> {
    let (reduction, role_axioms, el) = match roles {
        Roles::El(ax) => (None, candidates, Some(ax)),
        Roles::Original(red, original) => (Some((red, original)), original, None),
    };
    let mut s = Scan {
        roles: role_axioms,
        el,
        it,
        stop,
        ticks: 0,
        halted: false,
        path: [(Key::Fresh(u32::MAX), false); ROLE_DEPTH],
        path_len: 0,
    };
    for (i, k) in candidates.iter().enumerate() {
        if !s.tick() {
            return Err(Halt::Stopped);
        }
        let takes = s.tier_d_needs_counting(k)
            && match reduction {
                None => s.horn_takes(k),
                Some((red, original)) => s.horn_takes_origin(k, red, original),
            };
        if s.halted {
            return Err(Halt::Stopped);
        }
        if takes {
            // the run is rooted, as the normalization reads it, only with an individual
            if !it.individuals.is_empty() {
                return Ok(Some(i));
            }
            for k in candidates {
                match s.holds_individual(k) {
                    Some(true) => return Ok(Some(i)),
                    Some(false) => {}
                    None if s.halted => return Err(Halt::Stopped),
                    // past the nesting bound the walk does not tell, and does not refuse
                    None => return Ok(None),
                }
            }
            return Ok(None);
        }
    }
    Ok(None)
}

/// The positions of an expression in the axioms of the Tier D normalization: positive,
/// negative or both, as an equivalence or a disjoint union has its operands.
#[derive(Clone, Copy)]
struct Pol {
    pos: bool,
    neg: bool,
}

impl Pol {
    const POS: Pol = Pol {
        pos: true,
        neg: false,
    };
    const NEG: Pol = Pol {
        pos: false,
        neg: true,
    };
    const BOTH: Pol = Pol {
        pos: true,
        neg: true,
    };

    fn flip(self) -> Pol {
        Pol {
            pos: self.neg,
            neg: self.pos,
        }
    }
}

/// What the Horn normalization makes of an expression, as far as the check compares two
/// of them: two equal shapes normalize to the same expression of the interner, and two
/// that the check cannot compare are [`Shape::Opaque`].
#[derive(Clone, Copy, Default)]
enum Shape<'c> {
    Top,
    Bottom,
    Class(&'c Class),
    Nominal(&'c Individual),
    /// `∃p.f`, whose filler has the expression's position
    Exist(&'c ObjectPropertyExpression, Fill<'c>),
    /// `≤1 p.f` in a positive position, whose filler is in a negative one
    AtMost1(&'c ObjectPropertyExpression, Fill<'c>),
    /// an intersection of more than one operand besides `owl:Thing`
    Conj(&'c std::collections::BTreeSet<ClassExpression>),
    /// a fresh class, or what the check does not compare
    #[default]
    Opaque,
}

/// The filler of a restriction in a [`Shape`].
#[derive(Clone, Copy)]
enum Fill<'c> {
    Top,
    Expr(&'c ClassExpression),
    Nominal(&'c Individual),
}

fn fill(g: Option<&ClassExpression>) -> Fill<'_> {
    g.map_or(Fill::Top, Fill::Expr)
}

/// The shape of an expression in a positive (`pos`) or a negative position, as
/// `horn::Norm::pos` and `horn::Norm::neg` intern it. An intersection drops `owl:Thing`
/// and a union `owl:Nothing`, and one that keeps a single operand is that operand, as
/// both do. An existential restriction, a minimum of one and a value restriction are one
/// shape, and a maximum of one in a positive position another.
fn shape(c: &ClassExpression, pos: bool) -> Shape<'_> {
    match c {
        C::Class(k) => match class_v(k) {
            V::Top => Shape::Top,
            V::Bottom => Shape::Bottom,
            V::Other => Shape::Class(k),
        },
        C::ObjectIntersectionOf(cs) => {
            let mut rest = cs.iter().filter(|c| class_of(c) != Some(V::Top));
            match (rest.next(), rest.next()) {
                (None, _) => Shape::Top,
                (Some(c), None) => shape(c, pos),
                _ if cs.iter().any(|c| class_of(c) == Some(V::Bottom)) => Shape::Bottom,
                _ => Shape::Conj(cs),
            }
        }
        C::ObjectUnionOf(cs) => {
            if cs.iter().any(|c| class_of(c) == Some(V::Top)) {
                return Shape::Top;
            }
            let mut rest = cs.iter().filter(|c| class_of(c) != Some(V::Bottom));
            match (rest.next(), rest.next()) {
                (None, _) => Shape::Bottom,
                (Some(c), None) => shape(c, pos),
                _ => Shape::Opaque,
            }
        }
        C::ObjectSomeValuesFrom(p, g) => Shape::Exist(p, Fill::Expr(g)),
        C::ObjectMinCardinality(0, ..) => Shape::Top,
        C::ObjectMinCardinality(1, p, g) => Shape::Exist(p, fill(g.as_deref())),
        C::ObjectHasValue(p, a) => Shape::Exist(p, Fill::Nominal(a)),
        C::ObjectOneOf(is) if is.len() == 1 => {
            is.iter().next().map_or(Shape::Opaque, Shape::Nominal)
        }
        C::ObjectMaxCardinality(1, p, g) if pos => Shape::AtMost1(p, fill(g.as_deref())),
        _ => Shape::Opaque,
    }
}

/// The shape of a filler in a positive (`pos`) or a negative position.
fn fill_shape(f: Fill<'_>, pos: bool) -> Shape<'_> {
    match f {
        Fill::Top => Shape::Top,
        Fill::Expr(g) => shape(g, pos),
        Fill::Nominal(i) => Shape::Nominal(i),
    }
}

fn class_of(c: &ClassExpression) -> Option<V> {
    match c {
        C::Class(k) => Some(class_v(k)),
        _ => None,
    }
}

/// The shape of a disjunct of a Horn inclusion's head, in a positive position.
fn head_shape(h: Head<'_>) -> Shape<'_> {
    match h {
        Head::Expr(c) => shape(c, true),
        Head::Min(1, p, g) => Shape::Exist(p, fill(g)),
        Head::Max(1, p, g) => Shape::AtMost1(p, fill(g)),
        _ => Shape::Opaque,
    }
}

/// The value of an expression of a normalization, as far as its checks read it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum V {
    Top,
    Bottom,
    Other,
}

fn conj(a: V, b: V) -> V {
    match (a, b) {
        (V::Bottom, _) | (_, V::Bottom) => V::Bottom,
        (V::Top, x) | (x, V::Top) => x,
        _ => V::Other,
    }
}

fn class_v(c: &Class) -> V {
    match c.as_str() {
        vocab::OWL_THING => V::Top,
        vocab::OWL_NOTHING => V::Bottom,
        _ => V::Other,
    }
}

fn top_or_bottom(name: &str) -> bool {
    matches!(
        name,
        vocab::OWL_TOP_OBJECT_PROPERTY | vocab::OWL_BOTTOM_OBJECT_PROPERTY
    )
}

fn name_of(p: &ObjectPropertyExpression) -> (&str, bool) {
    match p {
        P::Property(n) => (n.as_str(), false),
        P::InverseOf(n) => (n.as_str(), true),
    }
}

/// A role of the role box: an object property, or a role that the binarization of a
/// chain introduces.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Key<'a> {
    Iri(&'a str),
    Fresh(u32),
}

/// What a Tier D walk found: a construct that needs counting, and a construct that the
/// normalization leaves out with counting too.
#[derive(Default)]
struct Td {
    counting: bool,
    other: bool,
}

/// What one Horn inclusion `⊓lhs ⊑ ⊔rhs` holds as its flattening reads it.
#[derive(Default)]
struct Sub<'c> {
    steps: usize,
    body_bottom: bool,
    head_top: bool,
    /// the distinct disjuncts of the head, two for two or more
    head: usize,
    /// the first disjunct of the head
    first: Shape<'c>,
}

impl Sub<'_> {
    fn left(&mut self, v: V) {
        if v == V::Bottom {
            self.body_bottom = true;
        }
    }

    /// Whether the Horn rules take the inclusion: one disjunct in the head at most, as the
    /// normalization removes repeats, unless the inclusion holds anyway.
    fn finish(&self) -> Option<()> {
        (self.head_top || self.body_bottom || self.head <= 1).then_some(())
    }
}

/// A disjunct of a Horn inclusion's head as the flattening moves it there from the body.
#[derive(Clone, Copy)]
enum Head<'c> {
    Expr(&'c ClassExpression),
    /// `∃p.¬f` for `∀p.f` in the body
    SomeNot(&'c ObjectPropertyExpression, &'c ClassExpression),
    /// `≥n p.f` for `≤(n-1) p.f` in the body
    Min(
        u32,
        &'c ObjectPropertyExpression,
        Option<&'c ClassExpression>,
    ),
    /// `≤n p.f` for `≥(n+1) p.f` in the body
    Max(
        u32,
        &'c ObjectPropertyExpression,
        Option<&'c ClassExpression>,
    ),
}

struct Scan<'a> {
    /// the axioms whose role axioms make the role box beside `el`'s
    roles: &'a [&'a AxiomKind],
    /// the EL axioms of a run over the original axioms
    el: Option<&'a Axioms>,
    it: &'a Interner,
    stop: &'a dyn Fn() -> bool,
    ticks: usize,
    halted: bool,
    /// the roles on the path of the search for a chain, which it does not enter again
    path: [(Key<'a>, bool); ROLE_DEPTH],
    path_len: usize,
}

impl<'a> Scan<'a> {
    /// One step, `false` once `stop` said the run's deadline or cancellation came.
    fn tick(&mut self) -> bool {
        if self.halted {
            return false;
        }
        self.ticks += 1;
        if self.ticks.is_multiple_of(POLL_TICKS) && (self.stop)() {
            self.halted = true;
        }
        !self.halted
    }

    /// The length of the run of operands equal to `s[i]` that starts at `i` in a sorted
    /// multiset, polling as it compares them, `None` once `stop` came.
    fn run(&mut self, s: &[ClassExpression], i: usize) -> Option<usize> {
        if !self.tick() {
            return None;
        }
        let mut j = i + 1;
        while j < s.len() && s[j] == s[i] {
            if !self.tick() {
                return None;
            }
            j += 1;
        }
        Some(j - i)
    }

    /// The operands of a sorted multiset that occur once, `None` once `stop` came.
    fn unique(&mut self, s: &[ClassExpression]) -> Option<usize> {
        let (mut i, mut unique) = (0, 0);
        while i < s.len() {
            let n = self.run(s, i)?;
            unique += usize::from(n == 1);
            i += n;
        }
        Some(unique)
    }

    /// Whether an axiom holds an individual outside its annotations, named or anonymous,
    /// as `index::holds_individual` reads a logical axiom, with no list of them. `None`
    /// once `stop` came or past the nesting bound.
    fn holds_individual(&mut self, k: &AxiomKind) -> Option<bool> {
        Some(match k {
            K::SubClassOf(a, b) => self.ce_individual(a, 0)? || self.ce_individual(b, 0)?,
            K::EquivalentClasses(cs) => self.any_individual(cs)?,
            K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => {
                self.any_individual(cs.as_slice())?
            }
            K::ObjectPropertyDomain(_, c)
            | K::ObjectPropertyRange(_, c)
            | K::DataPropertyDomain(_, c)
            | K::HasKey(c, _, _) => self.ce_individual(c, 0)?,
            K::ClassAssertion(..)
            | K::ObjectPropertyAssertion(..)
            | K::NegativeObjectPropertyAssertion(..)
            | K::DataPropertyAssertion(..)
            | K::NegativeDataPropertyAssertion(..) => true,
            K::SameIndividual(is) => !is.is_empty(),
            K::DifferentIndividuals(is) => !is.is_empty(),
            // a declaration of an individual is not logical, and the other axioms hold none
            _ => false,
        })
    }

    fn any_individual<'c>(
        &mut self,
        cs: impl IntoIterator<Item = &'c ClassExpression>,
    ) -> Option<bool> {
        for c in cs {
            if self.ce_individual(c, 0)? {
                return Some(true);
            }
        }
        Some(false)
    }

    fn ce_individual(&mut self, c: &ClassExpression, depth: usize) -> Option<bool> {
        if !self.tick() || depth > MAX_DEPTH {
            return None;
        }
        let d = depth + 1;
        Some(match c {
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                for c in cs {
                    if self.ce_individual(c, d)? {
                        return Some(true);
                    }
                }
                false
            }
            C::ObjectComplementOf(g)
            | C::ObjectSomeValuesFrom(_, g)
            | C::ObjectAllValuesFrom(_, g)
            | C::ObjectMinCardinality(_, _, Some(g))
            | C::ObjectMaxCardinality(_, _, Some(g))
            | C::ObjectExactCardinality(_, _, Some(g)) => self.ce_individual(g, d)?,
            C::ObjectOneOf(is) => !is.is_empty(),
            C::ObjectHasValue(..) => true,
            _ => false,
        })
    }

    // --- The Tier D normalization of a rooted run without counting ---

    /// Whether the rooted Tier D normalization without counting leaves the axiom out for
    /// a construct that needs counting and for nothing else.
    fn tier_d_needs_counting(&mut self, k: &AxiomKind) -> bool {
        let mut f = Td::default();
        match k {
            K::FunctionalObjectProperty(p)
            | K::InverseFunctionalObjectProperty(p)
            | K::AsymmetricObjectProperty(p) => {
                f.counting = true;
                self.td_role(p, &mut f);
            }
            K::DisjointObjectProperties(ps) => {
                f.counting = true;
                for p in ps.iter() {
                    if !self.tick() {
                        return false;
                    }
                    self.td_role(p, &mut f);
                }
            }
            K::SubClassOf(c, d) => {
                self.td_flat(c, d, &mut f);
                self.td(c, 0, Pol::NEG, &mut f);
                self.td(d, 0, Pol::POS, &mut f);
            }
            K::EquivalentClasses(cs) if cs.len() >= 2 => {
                let mut prev = cs.iter().next_back();
                for c in cs {
                    if let Some(p) = prev {
                        self.td_flat(p, c, &mut f);
                    }
                    prev = Some(c);
                    self.td(c, 0, Pol::BOTH, &mut f);
                }
            }
            K::DisjointClasses(cs) => {
                // `c ⊑ ⊥` for an operand that occurs more than once, and the pairwise
                // disjointness of those that occur once
                let cs = cs.as_slice();
                let Some(unique) = self.unique(cs) else {
                    return false;
                };
                let mut i = 0;
                while i < cs.len() {
                    let Some(n) = self.run(cs, i) else {
                        return false;
                    };
                    if n > 1 || unique >= 2 {
                        self.td(&cs[i], 0, Pol::NEG, &mut f);
                    }
                    i += n;
                }
            }
            K::DisjointUnion(_, ds) => {
                // `C ⊑ ⊔ds` with the operands that occur once, and `d ⊑ C` for each
                let ds = ds.as_slice();
                let (mut head, mut i) = (1, 0);
                while i < ds.len() {
                    let Some(n) = self.run(ds, i) else {
                        return false;
                    };
                    let c = &ds[i];
                    if n == 1 {
                        self.td_steps(c, true, &mut head, &mut f);
                        let mut steps = 1;
                        self.td_steps(c, false, &mut steps, &mut f);
                    }
                    self.td(c, 0, Pol::BOTH, &mut f);
                    i += n;
                }
            }
            K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => {
                self.td_role(p, &mut f);
                self.td(c, 0, Pol::POS, &mut f);
            }
            K::ClassAssertion(c, _) => self.td(c, 0, Pol::POS, &mut f),
            // the other axioms hold no construct that needs counting
            _ => return false,
        }
        f.counting && !f.other && !self.halted
    }

    fn td_role(&mut self, p: &ObjectPropertyExpression, f: &mut Td) {
        if top_or_bottom(p.named().as_str()) {
            f.other = true;
        }
    }

    /// The flattening of `c ⊑ d` as Tier D's `sub` reads it, which leaves out an inclusion
    /// past [`MAX_STEPS`]. The steps are those of a run with counting, which moves a
    /// minimum above one in the body to the head as well, and so at least those of a run
    /// without it.
    fn td_flat(&mut self, c: &ClassExpression, d: &ClassExpression, f: &mut Td) {
        let mut steps = 0;
        self.td_steps(c, false, &mut steps, f);
        self.td_steps(d, true, &mut steps, f);
    }

    fn td_steps(&mut self, c: &ClassExpression, right: bool, steps: &mut usize, f: &mut Td) {
        *steps += 1;
        if *steps > MAX_STEPS || !self.tick() {
            f.other = true;
        }
        if f.other {
            return;
        }
        match (c, right) {
            (C::ObjectIntersectionOf(cs), false) | (C::ObjectUnionOf(cs), true) => {
                for c in cs {
                    self.td_steps(c, right, steps, f);
                }
            }
            (C::ObjectComplementOf(g), _) => self.td_steps(g, !right, steps, f),
            // a universal, a maximum or, with counting, a minimum above one in the body is
            // a head disjunct of its own
            (C::ObjectAllValuesFrom(..), false) => self.td_step(steps, f),
            (C::ObjectMaxCardinality(n, ..), false) if n.checked_add(1).is_some() => {
                self.td_step(steps, f)
            }
            (C::ObjectMinCardinality(n, ..), false) if *n >= 2 => self.td_step(steps, f),
            _ => {}
        }
    }

    fn td_step(&mut self, steps: &mut usize, f: &mut Td) {
        *steps += 1;
        if *steps > MAX_STEPS {
            f.other = true;
        }
    }

    /// The Tier D normalization without counting of `c` in the positions `pol`. A rooted
    /// run leaves out a minimum above one, a maximum above zero and an exact cardinality
    /// above zero in either position, as a maximum in a negative position is the minimum
    /// above it in a positive one. The run with counting unfolds a minimum in a positive
    /// position and the dual of a maximum in a negative one before it reads the filler,
    /// and leaves either out past [`MAX_MIN`]. The depth grows at least as fast as the
    /// normalization's.
    fn td(&mut self, c: &ClassExpression, depth: usize, pol: Pol, f: &mut Td) {
        if f.other || !self.tick() || depth > MAX_DEPTH {
            f.other = true;
            return;
        }
        let d = depth + 1;
        match c {
            C::Class(_) | C::ObjectOneOf(_) | C::ObjectMinCardinality(0, ..) => {}
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                for c in cs {
                    self.td(c, d, pol, f);
                }
            }
            C::ObjectComplementOf(g) => self.td(g, d, pol.flip(), f),
            C::ObjectSomeValuesFrom(p, g) | C::ObjectAllValuesFrom(p, g) => {
                self.td_role(p, f);
                self.td(g, d, pol, f);
            }
            // `≥1 p.f` is `∃p.f`
            C::ObjectMinCardinality(1, p, g) => {
                self.td_role(p, f);
                if let Some(g) = g {
                    self.td(g, d + 1, pol, f);
                }
            }
            // `≤0 p.f` is `∀p.¬f`, and in a negative position its dual is `∃p.f`
            C::ObjectMaxCardinality(0, p, g) | C::ObjectExactCardinality(0, p, g) => {
                self.td_role(p, f);
                if let Some(g) = g {
                    self.td(g, d + 1, pol.flip(), f);
                }
            }
            // what needs counting
            C::ObjectMinCardinality(n, p, g)
            | C::ObjectMaxCardinality(n, p, g)
            | C::ObjectExactCardinality(n, p, g) => {
                f.counting = true;
                let (min, max) = match c {
                    C::ObjectMinCardinality(..) => (true, false),
                    C::ObjectMaxCardinality(..) => (false, true),
                    _ => (true, true),
                };
                if (min && pol.pos && *n > MAX_MIN) || (max && pol.neg && *n >= MAX_MIN) {
                    f.other = true;
                    return;
                }
                self.td_role(p, f);
                if let Some(g) = g {
                    self.td_any(g, d + 1, f);
                }
            }
            C::ObjectHasValue(p, _) => self.td_role(p, f),
            // self restrictions and data restrictions
            _ => f.other = true,
        }
    }

    /// Whether the Tier D normalization with counting leaves out something inside the
    /// filler of a number restriction, in either position. A number restriction past the
    /// bound of [`Scan::td`] counts as left out in any position.
    fn td_any(&mut self, c: &ClassExpression, depth: usize, f: &mut Td) {
        if f.other || !self.tick() || depth > MAX_DEPTH {
            f.other = true;
            return;
        }
        let d = depth + 1;
        match c {
            C::Class(_) | C::ObjectOneOf(_) | C::ObjectMinCardinality(0, ..) => {}
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                for c in cs {
                    self.td_any(c, d, f);
                }
            }
            C::ObjectComplementOf(g) => self.td_any(g, d, f),
            C::ObjectSomeValuesFrom(p, g) | C::ObjectAllValuesFrom(p, g) => {
                self.td_role(p, f);
                self.td_any(g, d, f);
            }
            C::ObjectMinCardinality(n, p, g)
            | C::ObjectMaxCardinality(n, p, g)
            | C::ObjectExactCardinality(n, p, g) => {
                let min = matches!(c, C::ObjectMinCardinality(..));
                if *n > MAX_MIN || (!min && *n == MAX_MIN) {
                    f.other = true;
                    return;
                }
                self.td_role(p, f);
                if let Some(g) = g {
                    self.td_any(g, d + 1, f);
                }
            }
            C::ObjectHasValue(p, _) => self.td_role(p, f),
            _ => f.other = true,
        }
    }

    // --- The Horn normalization ---

    /// Whether the Horn normalization takes the axiom.
    fn horn_takes(&mut self, k: &AxiomKind) -> bool {
        !self.halted && self.horn_axiom(k).is_some() && !self.halted
    }

    /// Whether the Horn normalization takes an original axiom that a rewritten one stands
    /// for, or the axiom itself when the ontology states it as it is.
    fn horn_takes_origin(
        &mut self,
        k: &AxiomKind,
        red: &super::dl_reduce::Reduction,
        original: &[&AxiomKind],
    ) -> bool {
        for (o, replaced) in original.iter().zip(&red.replaced) {
            if !self.tick() {
                return false;
            }
            if !*replaced && *o == k {
                return self.horn_takes(o);
            }
        }
        for (a, &i) in red.axioms.iter().zip(&red.origin) {
            if !self.tick() {
                return false;
            }
            if a == k && original.get(i).is_some_and(|o| self.horn_takes(o)) {
                return true;
            }
        }
        false
    }

    fn horn_axiom(&mut self, k: &AxiomKind) -> Option<()> {
        match k {
            K::SubClassOf(c, d) => {
                let mut s = Sub::default();
                self.h_left(c, &mut s)?;
                self.h_right(Head::Expr(d), &mut s)?;
                s.finish()
            }
            K::EquivalentClasses(cs) => {
                let mut prev = cs.iter().next_back();
                for c in cs {
                    if let Some(p) = prev
                        && p != c
                    {
                        let mut s = Sub::default();
                        self.h_left(p, &mut s)?;
                        self.h_right(Head::Expr(c), &mut s)?;
                        s.finish()?;
                    }
                    prev = Some(c);
                }
                Some(())
            }
            K::DisjointClasses(cs) => {
                let cs = cs.as_slice();
                let mut i = 0;
                while i < cs.len() {
                    let n = self.run(cs, i)?;
                    if n > 1 {
                        self.h_empty(&cs[i])?;
                    }
                    i += n;
                }
                self.h_disjoint(cs)
            }
            K::DisjointUnion(c, ds) => {
                let ds = ds.as_slice();
                let class = class_v(c);
                let mut s = Sub::default();
                self.h_step(&mut s)?;
                s.left(class);
                let mut i = 0;
                while i < ds.len() {
                    let n = self.run(ds, i)?;
                    if n == 1 {
                        self.h_right(Head::Expr(&ds[i]), &mut s)?;
                    }
                    i += n;
                }
                s.finish()?;
                let mut i = 0;
                while i < ds.len() {
                    let n = self.run(ds, i)?;
                    if n == 1 {
                        let mut s = Sub::default();
                        self.h_left(&ds[i], &mut s)?;
                        self.h_step(&mut s)?;
                        self.h_head(&mut s, class, Shape::Opaque);
                        s.finish()?;
                    } else {
                        self.h_empty(&ds[i])?;
                    }
                    i += n;
                }
                self.h_disjoint(ds)
            }
            // `∃p.⊤ ⊑ c`
            K::ObjectPropertyDomain(p, c) => {
                let mut s = Sub::default();
                self.h_step(&mut s)?;
                self.h_role(p)?;
                s.left(V::Other);
                self.h_right(Head::Expr(c), &mut s)?;
                s.finish()
            }
            K::ObjectPropertyRange(p, c) => {
                self.h_role(p)?;
                self.h_pos(c, 0).map(|_| ())
            }
            // `{a} ⊑ c`
            K::ClassAssertion(c, _) => {
                let mut s = Sub::default();
                self.h_step(&mut s)?;
                s.left(V::Other);
                self.h_right(Head::Expr(c), &mut s)?;
                s.finish()
            }
            K::FunctionalObjectProperty(p) => self.h_simple(name_of(p)),
            K::InverseFunctionalObjectProperty(p) => {
                let (n, inv) = name_of(p);
                self.h_simple((n, !inv))
            }
            K::AsymmetricObjectProperty(p) => {
                let (n, inv) = name_of(p);
                self.h_simple((n, inv))?;
                self.h_simple((n, !inv))
            }
            K::DisjointObjectProperties(ps) => {
                for p in ps.iter() {
                    self.h_simple(name_of(p))?;
                }
                Some(())
            }
            _ => None,
        }
    }

    /// `c ⊑ ⊥`: the negative form of `c`, or the inclusion when it has none.
    fn h_empty(&mut self, c: &ClassExpression) -> Option<()> {
        if self.h_neg(c, 0).is_some() {
            return Some(());
        }
        if self.halted {
            return None;
        }
        let mut s = Sub::default();
        self.h_left(c, &mut s)?;
        s.finish()
    }

    /// The pairwise disjointness of the operands that occur once, with one disjointness
    /// set when every one has a negative form and an inclusion for each pair otherwise.
    fn h_disjoint(&mut self, cs: &[ClassExpression]) -> Option<()> {
        if self.unique(cs)? < 2 {
            return Some(());
        }
        let (mut all, mut i) = (true, 0);
        while i < cs.len() {
            let n = self.run(cs, i)?;
            if n == 1 && self.h_neg(&cs[i], 0).is_none() {
                all = false;
                break;
            }
            i += n;
        }
        if self.halted {
            return None;
        }
        if all {
            return Some(());
        }
        let mut i = 0;
        while i < cs.len() {
            let n = self.run(cs, i)?;
            let mut j = i + n;
            while n == 1 && j < cs.len() {
                let m = self.run(cs, j)?;
                if m == 1 {
                    let mut s = Sub::default();
                    self.h_left(&cs[i], &mut s)?;
                    self.h_left(&cs[j], &mut s)?;
                    s.finish()?;
                }
                j += m;
            }
            i += n;
        }
        Some(())
    }

    /// A disjunct of a Horn inclusion's head with its value and shape. A disjunct whose
    /// shape is that of the first does not count again, as the normalization removes
    /// repeats of an interned expression.
    fn h_head<'c>(&mut self, s: &mut Sub<'c>, v: V, h: Shape<'c>) {
        match v {
            V::Top => s.head_top = true,
            V::Bottom => {}
            V::Other => match s.head {
                0 => {
                    s.head = 1;
                    s.first = h;
                }
                1 if self.same(s.first, h, true, &mut 0) => {}
                _ => s.head = 2,
            },
        }
    }

    /// Whether two shapes in a positive (`pos`) or a negative position normalize to the
    /// same expression, as far as the check tells. It answers `false` when it cannot tell,
    /// past [`SAME_WORK`] comparisons or once `stop` came, which keeps the repeats apart
    /// and the Tier D run as it was.
    fn same<'c>(&mut self, a: Shape<'c>, b: Shape<'c>, pos: bool, work: &mut usize) -> bool {
        *work += 1;
        if *work > SAME_WORK || !self.tick() {
            return false;
        }
        match (a, b) {
            (Shape::Top, Shape::Top) | (Shape::Bottom, Shape::Bottom) => true,
            (Shape::Class(x), Shape::Class(y)) => x == y,
            (Shape::Nominal(x), Shape::Nominal(y)) => x == y,
            (Shape::Exist(p, f), Shape::Exist(q, g)) => {
                p == q && self.same(fill_shape(f, pos), fill_shape(g, pos), pos, work)
            }
            // the filler of a maximum is in a negative position
            (Shape::AtMost1(p, f), Shape::AtMost1(q, g)) => {
                p == q && self.same(fill_shape(f, false), fill_shape(g, false), false, work)
            }
            // the same operands besides `owl:Thing`, each of one a repeat of one of the other
            (Shape::Conj(xs), Shape::Conj(ys)) => {
                self.within(xs, ys, pos, work) && self.within(ys, xs, pos, work)
            }
            _ => false,
        }
    }

    /// Whether every operand of `xs` besides `owl:Thing` has the shape of one of `ys`.
    fn within<'c>(
        &mut self,
        xs: &'c std::collections::BTreeSet<ClassExpression>,
        ys: &'c std::collections::BTreeSet<ClassExpression>,
        pos: bool,
        work: &mut usize,
    ) -> bool {
        let thing = |c: &ClassExpression| class_of(c) == Some(V::Top);
        xs.iter().filter(|x| !thing(x)).all(|x| {
            ys.iter()
                .filter(|y| !thing(y))
                .any(|y| self.same(shape(x, pos), shape(y, pos), pos, work))
        })
    }

    fn h_step(&mut self, s: &mut Sub<'_>) -> Option<()> {
        s.steps += 1;
        (s.steps <= MAX_STEPS && self.tick()).then_some(())
    }

    /// A conjunct of a Horn inclusion's body, as `horn::Norm::sub` flattens it.
    fn h_left<'c>(&mut self, c: &'c ClassExpression, s: &mut Sub<'c>) -> Option<()> {
        self.h_step(s)?;
        match c {
            C::ObjectIntersectionOf(cs) => {
                for c in cs {
                    self.h_left(c, s)?;
                }
            }
            C::ObjectComplementOf(g) => self.h_right(Head::Expr(g), s)?,
            C::ObjectAllValuesFrom(p, g) => self.h_right(Head::SomeNot(p, g), s)?,
            C::ObjectMaxCardinality(n, p, g) => {
                let m = n.checked_add(1)?;
                self.h_right(Head::Min(m, p, g.as_deref()), s)?;
            }
            C::ObjectMinCardinality(n, p, g) if *n >= 2 => {
                self.h_right(Head::Max(n - 1, p, g.as_deref()), s)?;
            }
            C::Class(k) if k.as_str() == vocab::OWL_THING => {}
            c => {
                let v = self.h_neg(c, 0)?;
                s.left(v);
            }
        }
        Some(())
    }

    /// A disjunct of a Horn inclusion's head, as `horn::Norm::sub` flattens it.
    fn h_right<'c>(&mut self, h: Head<'c>, s: &mut Sub<'c>) -> Option<()> {
        self.h_step(s)?;
        let v = match h {
            Head::Expr(C::ObjectUnionOf(cs)) => {
                for c in cs {
                    self.h_right(Head::Expr(c), s)?;
                }
                return Some(());
            }
            Head::Expr(C::ObjectComplementOf(g)) => return self.h_left(g, s),
            Head::Expr(C::Class(k)) if k.as_str() == vocab::OWL_NOTHING => return Some(()),
            Head::Expr(c) => self.h_pos(c, 0)?,
            // `∃p.¬f` at depth 0, whose complement is at depth 1 and its operand at 2
            Head::SomeNot(p, g) => {
                self.h_role(p)?;
                match self.h_neg(g, 2)? {
                    V::Top => V::Bottom,
                    _ => V::Other,
                }
            }
            Head::Min(n, p, g) => self.h_at_least(n, p, g, 1)?,
            Head::Max(n, p, g) => self.h_at_most(n, p, g, 1)?,
        };
        self.h_head(s, v, head_shape(h));
        Some(())
    }

    fn h_role(&mut self, p: &ObjectPropertyExpression) -> Option<()> {
        (p.named().as_str() != vocab::OWL_BOTTOM_OBJECT_PROPERTY).then_some(())
    }

    /// `horn::Norm::pos`: `None` when it leaves the expression out. Data restrictions are
    /// not followed, since the Tier D walk leaves their axioms out of the refusal.
    fn h_pos(&mut self, c: &ClassExpression, depth: usize) -> Option<V> {
        if !self.tick() || depth > MAX_DEPTH {
            return None;
        }
        let d = depth + 1;
        Some(match c {
            C::Class(k) => class_v(k),
            C::ObjectIntersectionOf(cs) => {
                let mut v = V::Top;
                for c in cs {
                    v = conj(v, self.h_pos(c, d)?);
                }
                v
            }
            C::ObjectUnionOf(cs) => {
                // the distinct disjuncts, as the normalization removes repeats
                let (mut top, mut n, mut first) = (false, 0, Shape::Opaque);
                for c in cs {
                    match self.h_pos(c, d)? {
                        V::Top => top = true,
                        V::Bottom => {}
                        V::Other if n == 0 => (n, first) = (1, shape(c, true)),
                        V::Other if n == 1 && self.same(first, shape(c, true), true, &mut 0) => {}
                        V::Other => n = 2,
                    }
                }
                match (top, n) {
                    (true, _) => V::Top,
                    (_, 0) => V::Bottom,
                    (_, 1) => V::Other,
                    _ => return None,
                }
            }
            C::ObjectComplementOf(g) => match self.h_neg(g, d)? {
                V::Top => V::Bottom,
                V::Bottom => V::Top,
                V::Other => V::Other,
            },
            C::ObjectSomeValuesFrom(p, g) => {
                self.h_role(p)?;
                match self.h_pos(g, d)? {
                    V::Bottom => V::Bottom,
                    _ => V::Other,
                }
            }
            C::ObjectAllValuesFrom(p, g) => {
                self.h_role(p)?;
                match self.h_pos(g, d)? {
                    V::Top => V::Top,
                    _ => V::Other,
                }
            }
            C::ObjectHasSelf(p) => {
                self.h_simple(name_of(p))?;
                V::Other
            }
            C::ObjectMinCardinality(n, p, g) => self.h_at_least(*n, p, g.as_deref(), d)?,
            C::ObjectMaxCardinality(n, p, g) => self.h_at_most(*n, p, g.as_deref(), d)?,
            C::ObjectExactCardinality(n, p, g) => {
                let a = self.h_at_least(*n, p, g.as_deref(), d)?;
                let b = self.h_at_most(*n, p, g.as_deref(), d)?;
                conj(a, b)
            }
            C::ObjectOneOf(is) if is.len() == 1 => V::Other,
            C::ObjectHasValue(p, _) => {
                self.h_role(p)?;
                V::Other
            }
            _ => return None,
        })
    }

    /// `horn::Norm::neg`, as [`Scan::h_pos`] reads it.
    fn h_neg(&mut self, c: &ClassExpression, depth: usize) -> Option<V> {
        if !self.tick() || depth > MAX_DEPTH {
            return None;
        }
        let d = depth + 1;
        Some(match c {
            C::Class(k) => class_v(k),
            C::ObjectIntersectionOf(cs) => {
                let mut v = V::Top;
                for c in cs {
                    v = conj(v, self.h_neg(c, d)?);
                }
                v
            }
            C::ObjectUnionOf(cs) => {
                let (mut top, mut n) = (false, 0);
                for c in cs {
                    match self.h_neg(c, d)? {
                        V::Top => top = true,
                        V::Bottom => {}
                        V::Other => n += 1,
                    }
                }
                match (top, n) {
                    (true, _) => V::Top,
                    (_, 0) => V::Bottom,
                    _ => V::Other,
                }
            }
            C::ObjectSomeValuesFrom(p, g) => {
                self.h_role(p)?;
                match self.h_neg(g, d)? {
                    V::Bottom => V::Bottom,
                    _ => V::Other,
                }
            }
            C::ObjectMinCardinality(0, ..) => V::Top,
            C::ObjectMinCardinality(1, p, g) => {
                self.h_role(p)?;
                let v = match g {
                    Some(g) => self.h_neg(g, d)?,
                    None => V::Top,
                };
                match v {
                    V::Bottom => V::Bottom,
                    _ => V::Other,
                }
            }
            C::ObjectHasSelf(p) => {
                self.h_simple(name_of(p))?;
                V::Other
            }
            C::ObjectOneOf(_) => V::Other,
            C::ObjectHasValue(p, _) => {
                self.h_role(p)?;
                V::Other
            }
            _ => return None,
        })
    }

    /// `horn::Norm::at_least`.
    fn h_at_least(
        &mut self,
        n: u32,
        p: &ObjectPropertyExpression,
        g: Option<&ClassExpression>,
        d: usize,
    ) -> Option<V> {
        if n == 0 {
            return Some(V::Top);
        }
        if n == 1 {
            self.h_role(p)?;
        } else {
            self.h_simple(name_of(p))?;
        }
        let fill = match g {
            Some(g) => self.h_pos(g, d)?,
            None => V::Top,
        };
        if fill == V::Bottom {
            return Some(V::Bottom);
        }
        (n <= MAX_AT_LEAST).then_some(V::Other)
    }

    /// `horn::Norm::at_most`.
    fn h_at_most(
        &mut self,
        n: u32,
        p: &ObjectPropertyExpression,
        g: Option<&ClassExpression>,
        d: usize,
    ) -> Option<V> {
        self.h_simple(name_of(p))?;
        let g = match g {
            Some(g) => self.h_neg(g, d)?,
            None => V::Top,
        };
        if g == V::Bottom {
            return Some(V::Top);
        }
        (n <= 1).then_some(V::Other)
    }

    // --- The simple roles of the Horn role box ---

    /// Whether the Horn rules read the role as simple, which they ask of number
    /// restrictions, self restrictions and the role axioms that need counting.
    fn h_simple(&mut self, (name, inv): (&str, bool)) -> Option<()> {
        if top_or_bottom(name) {
            return None;
        }
        let mut work = 0;
        (!self.below_chain(Key::Iri(name), inv, &mut work)).then_some(())
    }

    fn role(&self, r: Role) -> Option<(Key<'a>, bool)> {
        match self.it.roles.get(r as usize)? {
            RoleName::Object(p) => Some((Key::Iri(p.as_str()), false)),
            RoleName::Inverse(p) => Some((Key::Iri(p.as_str()), true)),
            RoleName::Fresh(n) => Some((Key::Fresh(*n), false)),
            RoleName::Data(_) => None,
        }
    }

    /// Whether a role is below the target of a chain or of its mirror image, through the
    /// role inclusions closed under inverses, as `horn::RoleBox::new` marks the roles that
    /// are not simple. The role axioms are those the Horn rules take, which leave out the
    /// top property's and the bottom property's. An equivalence of `k` properties is the
    /// ring of `k` inclusions that the Horn rules build, so that the search reads it in
    /// linear steps. Every step counts toward [`ROLE_WORK`] and polls `stop`. The search
    /// does not enter a role on its path again, and answers `true` when it passes
    /// [`ROLE_DEPTH`] or [`ROLE_WORK`] or `stop` comes, so that a role it does not settle
    /// is not simple.
    fn below_chain(&mut self, name: Key<'_>, inv: bool, work: &mut usize) -> bool {
        self.below_chain_from(name, inv, (name, inv), work)
    }

    /// One role of the search below the one it started from, `root`.
    fn visit(&mut self, name: Key<'a>, inv: bool, root: (Key<'_>, bool), work: &mut usize) -> bool {
        if (name, inv) == root
            || self.path[..self.path_len]
                .iter()
                .any(|&(n, i)| n == name && i == inv)
        {
            return false;
        }
        if self.path_len >= ROLE_DEPTH {
            return true;
        }
        self.path[self.path_len] = (name, inv);
        self.path_len += 1;
        let found = self.below_chain_from(name, inv, root, work);
        self.path_len -= 1;
        found
    }

    fn below_chain_from(
        &mut self,
        name: Key<'_>,
        inv: bool,
        root: (Key<'_>, bool),
        work: &mut usize,
    ) -> bool {
        if *work > ROLE_WORK || !self.tick() {
            return true;
        }
        let (el, roles) = (self.el, self.roles);
        let iri = |p: &'a ObjectPropertyExpression| Key::Iri(p.named().as_str());
        let plain = |p: &ObjectPropertyExpression| !top_or_bottom(p.named().as_str());
        // a chain's target and its mirror's are not simple, nor what is above them
        for (_, t) in el.map_or(&[][..], |a| &a.chains[..]) {
            if self.charge(work) || self.role(*t).is_some_and(|(n, _)| n == name) {
                return true;
            }
        }
        for &k in roles {
            if self.charge(work) {
                return true;
            }
            match k {
                K::TransitiveObjectProperty(p) if plain(p) && iri(p) == name => return true,
                K::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), s)
                    if ps.len() >= 2 && plain(s) && iri(s) == name =>
                {
                    // the Horn rules leave out a chain through the top or bottom property
                    match self.all_plain(ps.iter(), work) {
                        Some(false) => {}
                        _ => return true,
                    }
                }
                _ => {}
            }
        }
        // the inclusions `a ⊑ b` into the role, with their mirrors `a⁻ ⊑ b⁻`
        let edge = |s: &mut Self, a: (Key<'a>, bool), b: (Key<'a>, bool), work: &mut usize| {
            b.0 == name && s.visit(a.0, a.1 ^ b.1 ^ inv, root, work)
        };
        let subs = el.map_or(&[][..], |a| &a.role_subs[..]);
        for &(a, b) in subs {
            if self.charge(work) {
                return true;
            }
            if let (Some(a), Some(b)) = (self.role(a), self.role(b))
                && edge(self, a, b, work)
            {
                return true;
            }
        }
        let key = |p: &'a ObjectPropertyExpression| {
            let (n, i) = name_of(p);
            (Key::Iri(n), i)
        };
        let flip = |(n, i): (Key<'a>, bool)| (n, !i);
        for &k in roles {
            if self.charge(work) {
                return true;
            }
            let found = match k {
                K::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(p), s)
                    if plain(p) && plain(s) =>
                {
                    edge(self, key(p), key(s), work)
                }
                K::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), s)
                    if ps.len() == 1 && plain(&ps[0]) && plain(s) =>
                {
                    edge(self, key(&ps[0]), key(s), work)
                }
                // the ring `p_i ⊑ p_(i+1)` that the Horn rules build, one edge per property
                K::EquivalentObjectProperties(ps) => match self.all_plain(ps.iter(), work) {
                    None => true,
                    Some(false) => false,
                    Some(true) => {
                        let mut prev = ps.iter().next_back();
                        let mut found = false;
                        for b in ps {
                            if self.charge(work) {
                                return true;
                            }
                            if let Some(a) = prev
                                && edge(self, key(a), key(b), work)
                            {
                                found = true;
                                break;
                            }
                            prev = Some(b);
                        }
                        found
                    }
                },
                // p ⊑ q⁻ and q⁻ ⊑ p
                K::InverseObjectProperties(p, q) if plain(p) && plain(q) => {
                    edge(self, key(p), flip(key(q)), work) || edge(self, flip(key(q)), key(p), work)
                }
                K::SymmetricObjectProperty(p) if plain(p) => edge(self, key(p), flip(key(p)), work),
                _ => false,
            };
            if found {
                return true;
            }
        }
        false
    }

    /// One step of the search for a chain, `true` once it passes [`ROLE_WORK`] or `stop`
    /// came.
    fn charge(&mut self, work: &mut usize) -> bool {
        *work += 1;
        *work > ROLE_WORK || !self.tick()
    }

    /// Whether every property of a role axiom is neither the top nor the bottom property,
    /// `None` once the search passes [`ROLE_WORK`] or `stop` came.
    fn all_plain<'p>(
        &mut self,
        ps: impl Iterator<Item = &'p ObjectPropertyExpression>,
        work: &mut usize,
    ) -> Option<bool> {
        for p in ps {
            if self.charge(work) {
                return None;
            }
            if top_or_bottom(p.named().as_str()) {
                return Some(false);
            }
        }
        Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(n: &str) -> ClassExpression {
        C::Class(Class::new(format!("http://example.org/{n}")))
    }

    fn prop(n: &str) -> ObjectPropertyExpression {
        P::Property(ObjectProperty::new(format!("http://example.org/{n}")))
    }

    fn min(n: u32, p: &str, f: Option<ClassExpression>) -> ClassExpression {
        C::ObjectMinCardinality(n, prop(p), f.map(Box::new))
    }

    fn max(n: u32, p: &str, f: Option<ClassExpression>) -> ClassExpression {
        C::ObjectMaxCardinality(n, prop(p), f.map(Box::new))
    }

    fn sub(c: ClassExpression, d: ClassExpression) -> AxiomKind {
        K::SubClassOf(c, d)
    }

    fn refuses_with(axioms: &[AxiomKind], stop: &dyn Fn() -> bool) -> Result<bool, Halt> {
        let mut it = Interner::default();
        it.individual(&Individual::named("http://example.org/a"));
        let refs: Vec<&AxiomKind> = axioms.iter().collect();
        refuses(&refs, &it, Roles::El(&Axioms::default()), stop)
    }

    fn check(axioms: &[AxiomKind]) -> bool {
        refuses_with(axioms, &|| false).unwrap()
    }

    #[test]
    fn a_horn_minimum_above_one_refuses() {
        assert!(check(&[sub(class("A"), min(2, "r", None))]));
        assert!(check(&[sub(class("A"), min(64, "r", Some(class("B"))))]));
        assert!(check(&[K::FunctionalObjectProperty(prop("r"))]));
        assert!(check(&[sub(class("A"), max(1, "r", Some(class("B"))))]));
    }

    fn nothing() -> ClassExpression {
        C::Class(Class::new(vocab::OWL_NOTHING))
    }

    fn and(cs: impl IntoIterator<Item = ClassExpression>) -> ClassExpression {
        C::ObjectIntersectionOf(cs.into_iter().collect())
    }

    fn or(cs: impl IntoIterator<Item = ClassExpression>) -> ClassExpression {
        C::ObjectUnionOf(cs.into_iter().collect())
    }

    fn some(p: &str, f: ClassExpression) -> ClassExpression {
        C::ObjectSomeValuesFrom(prop(p), Box::new(f))
    }

    /// An empty filler makes a minimum empty for the Horn rules at any number, while the
    /// rooted rules with counting unfold a minimum in a positive position, or the dual of a
    /// maximum in a negative one, before they read its filler, and leave it out past 64.
    #[test]
    fn a_bottom_minimum_refuses_up_to_the_tier_d_bound() {
        assert!(check(&[sub(class("A"), min(64, "r", Some(nothing())))]));
        assert!(check(&[sub(max(63, "r", Some(nothing())), class("A"))]));
        // a minimum in a negative position is the maximum below it, which has no bound
        assert!(check(&[sub(min(65, "r", Some(nothing())), class("A"))]));
        assert!(!check(&[sub(class("A"), min(65, "r", Some(nothing())))]));
        assert!(!check(&[sub(max(64, "r", Some(nothing())), class("A"))]));
        assert!(!check(&[sub(
            class("A"),
            min(2, "r", Some(min(65, "s", Some(nothing()))))
        )]));
    }

    /// Disjuncts that the Horn normalization interns as one expression are one disjunct.
    #[test]
    fn disjuncts_with_one_normalized_form_are_one() {
        let b = || class("B");
        let union = or([some("r", b()), min(1, "r", Some(b()))]);
        assert!(check(&[sub(class("A"), and([min(2, "s", None), union]))]));
        let head = or([
            max(1, "r", Some(b())),
            max(1, "r", Some(and([b(), C::Class(Class::thing())]))),
        ]);
        assert!(check(&[sub(class("A"), head)]));
        // `∃r.⊤`, a minimum of one without a filler and a value restriction's nominal
        let a = || Individual::named("http://example.org/a");
        let union = or([
            some("r", C::Class(Class::thing())),
            min(1, "r", None),
            min(2, "s", None),
        ]);
        assert!(!check(&[sub(class("A"), union)]));
        let union = or([
            C::ObjectHasValue(prop("r"), a()),
            some("r", C::ObjectOneOf([a()].into_iter().collect())),
        ]);
        assert!(check(&[sub(class("A"), and([min(2, "s", None), union]))]));
        // two intersections of the same operands in another order
        let union = or([
            some("r", and([b(), class("C")])),
            some("r", and([class("C"), b(), C::Class(Class::thing())])),
        ]);
        assert!(check(&[sub(class("A"), and([min(2, "s", None), union]))]));
        // distinct disjuncts are not Horn
        let union = or([some("r", b()), some("r", class("C"))]);
        assert!(!check(&[sub(class("A"), and([min(2, "s", None), union]))]));
        let head = or([max(1, "r", Some(b())), max(1, "r", Some(class("C")))]);
        assert!(!check(&[sub(class("A"), head)]));
        let head = or([max(1, "r", Some(b())), max(1, "s", Some(b()))]);
        assert!(!check(&[sub(class("A"), head)]));
    }

    #[test]
    fn a_minimum_that_neither_normalization_takes_does_not_refuse() {
        assert!(!check(&[sub(class("A"), min(65, "r", None))]));
        let bottom = P::Property(ObjectProperty::new(vocab::OWL_BOTTOM_OBJECT_PROPERTY));
        assert!(!check(&[K::FunctionalObjectProperty(bottom.clone())]));
        assert!(!check(&[sub(
            class("A"),
            C::ObjectMaxCardinality(1, bottom, None)
        )]));
        // a maximum above one in a positive position is not Horn
        assert!(!check(&[sub(class("A"), max(2, "r", None))]));
        // the Horn rules have no negative form of a minimum above one inside a restriction
        assert!(!check(&[sub(
            C::ObjectSomeValuesFrom(prop("r"), Box::new(min(2, "s", Some(class("B"))))),
            class("C"),
        )]));
    }

    #[test]
    fn a_zero_minimum_hides_its_filler() {
        let zero = min(0, "r", Some(max(1, "s", Some(class("A")))));
        assert!(!check(&[sub(zero.clone(), C::ObjectHasSelf(prop("t")))]));
        let data = C::DataSomeValuesFrom(
            vec![DataProperty::new("http://example.org/d")],
            DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
        );
        assert!(!check(&[sub(zero.clone(), data)]));
        assert!(!check(&[sub(zero, class("B"))]));
    }

    #[test]
    fn a_counting_axiom_with_a_self_restriction_does_not_refuse() {
        assert!(!check(&[sub(
            C::ObjectHasSelf(prop("t")),
            min(2, "r", None)
        )]));
    }

    #[test]
    fn a_role_below_a_chain_is_not_simple() {
        let transitive = K::TransitiveObjectProperty(prop("s"));
        let below = K::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(prop("s")),
            prop("r"),
        );
        let functional = K::FunctionalObjectProperty(prop("r"));
        assert!(check(std::slice::from_ref(&functional)));
        assert!(!check(&[
            functional.clone(),
            transitive.clone(),
            below.clone()
        ]));
        // the inverse of a role that is not simple is not simple either
        let inverse = K::InverseFunctionalObjectProperty(prop("s"));
        assert!(!check(&[inverse, transitive.clone()]));
        // a cycle of equivalences ends at the search's bound
        let eq = K::EquivalentObjectProperties(vec![prop("r"), prop("q")].into_iter().collect());
        assert!(check(&[functional.clone(), eq.clone()]));
        assert!(!check(&[functional, eq, transitive, below]));
    }

    #[test]
    fn the_check_stops_when_asked_on_a_wide_input() {
        let wide: Vec<AxiomKind> = (0..10_000)
            .map(|i| sub(class(&format!("A{i}")), class(&format!("B{i}"))))
            .chain(std::iter::once(sub(class("Z"), min(2, "r", None))))
            .collect();
        let calls = std::cell::Cell::new(0usize);
        let stop = || {
            calls.set(calls.get() + 1);
            calls.get() > 3
        };
        assert!(matches!(refuses_with(&wide, &stop), Err(Halt::Stopped)));
        // it polls every POLL_TICKS steps, not every step
        assert_eq!(calls.get(), 4);
        assert!(refuses_with(&wide, &|| false).unwrap());
    }

    /// One equivalence of many properties, one of them an inverse, beside a functional
    /// property that is not among them: the search reads the equivalence in linear steps
    /// and polls inside it.
    #[test]
    fn the_check_stops_inside_one_wide_role_equivalence() {
        let wide = K::EquivalentObjectProperties(
            (0..100_000)
                .map(|i| prop(&format!("q{i}")))
                .chain([P::InverseOf(ObjectProperty::new("http://example.org/q0"))])
                .collect(),
        );
        let axioms = [K::FunctionalObjectProperty(prop("f")), wide];
        let calls = std::cell::Cell::new(0usize);
        let stop = || {
            calls.set(calls.get() + 1);
            calls.get() > 3
        };
        assert!(matches!(refuses_with(&axioms, &stop), Err(Halt::Stopped)));
        assert_eq!(calls.get(), 4);
        assert!(refuses_with(&axioms, &|| false).unwrap());
        // a property in the ring is simple too, through the ring's inclusions
        let ring = K::EquivalentObjectProperties(["p", "q", "s"].into_iter().map(prop).collect());
        assert!(check(&[
            K::FunctionalObjectProperty(prop("q")),
            ring.clone()
        ]));
        let below = K::TransitiveObjectProperty(prop("s"));
        assert!(!check(&[
            K::FunctionalObjectProperty(prop("q")),
            ring,
            below
        ]));
        // the search follows the ring one inclusion at a time, so that a member of an
        // equivalence wider than its depth bound is a role it does not settle
        let ring = |k: usize| {
            K::EquivalentObjectProperties((0..k).map(|i| prop(&format!("q{i}"))).collect())
        };
        let functional = K::FunctionalObjectProperty(prop("q0"));
        assert!(check(&[functional.clone(), ring(ROLE_DEPTH)]));
        assert!(!check(&[functional, ring(ROLE_DEPTH + 2)]));
    }

    /// Without an individual in the interner, the check finds one in the axioms, named or
    /// anonymous and in any position the normalization reads, and not in a declaration.
    #[test]
    fn the_check_finds_an_individual_in_the_axioms() {
        let counting = sub(class("Z"), min(2, "r", None));
        let check_empty = |extra: AxiomKind| {
            let axioms = [counting.clone(), extra];
            let refs: Vec<&AxiomKind> = axioms.iter().collect();
            refuses(
                &refs,
                &Interner::default(),
                Roles::El(&Axioms::default()),
                &|| false,
            )
            .unwrap()
        };
        let nested = |i: Individual| {
            sub(
                class("B"),
                some(
                    "r",
                    min(1, "s", Some(C::ObjectOneOf([i].into_iter().collect()))),
                ),
            )
        };
        assert!(check_empty(nested(Individual::anonymous("x"))));
        assert!(check_empty(nested(Individual::named(
            "http://example.org/a"
        ))));
        assert!(check_empty(K::DisjointClasses(
            [
                class("B"),
                C::ObjectHasValue(prop("r"), Individual::anonymous("x"))
            ]
            .into_iter()
            .collect()
        )));
        assert!(!check_empty(sub(class("B"), class("C"))));
        assert!(!check_empty(K::Declaration(Entity::NamedIndividual(
            NamedIndividual::new("http://example.org/a")
        ))));
    }

    /// The ontology's EL axioms and interner, and its axioms outside OWL 2 EL with the
    /// context of a disjunction and an individual, which make a rooted Tier D run.
    fn built(input: &[Axiom]) -> (Interner, Axioms, Vec<AxiomKind>) {
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        let mut outside = Vec::new();
        // the axioms outside the profile are the candidates, as the build reads them
        for a in input {
            if super::super::logical(&a.kind) && super::super::el_break(&a.kind).is_some() {
                outside.push(a.kind.clone());
            } else {
                let _ = super::super::index::normalize(&a.kind, &mut it, &mut ax);
            }
        }
        let zz = |n: &str| C::Class(Class::new(format!("urn:dl1286-ctx:{n}")));
        outside.push(sub(
            zz("Z"),
            C::ObjectUnionOf([zz("Z1"), zz("Z2")].into_iter().collect()),
        ));
        it.individual(&Individual::named("urn:dl1286-ctx:z"));
        outside.sort();
        outside.dedup();
        (it, ax, outside)
    }

    /// The flags of the rooted Tier D normalization, `None` when it takes no axiom that
    /// needs case analysis.
    fn tier_d(input: &[Axiom], counting: bool) -> Option<Vec<bool>> {
        let (mut it, mut ax, outside) = built(input);
        let refs: Vec<&AxiomKind> = outside.iter().collect();
        let allowed = super::super::dl_norm::Allowed {
            counting,
            rooted: true,
            ..Default::default()
        };
        super::super::dl_norm::normalize(&refs, &mut it, &mut ax, None, &|| false, allowed)
            .unwrap()
            .map(|(flags, _)| flags)
    }

    /// The check against the normalizations themselves. Each axiom it refuses for is one
    /// that the rooted Tier D normalization without counting leaves out, that the one
    /// with counting takes when it answers, and that the Horn normalization takes, over
    /// the 640 fixtures of item 603 and synthetic cases.
    #[test]
    fn the_check_agrees_with_both_normalizations() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".ofn") && n.matches('.').count() == 1)
            })
            .collect();
        paths.sort();
        assert_eq!(paths.len(), 640);
        let mut inputs: Vec<(String, Vec<Axiom>)> = paths
            .iter()
            .map(|p| {
                let text = std::fs::read_to_string(p).unwrap();
                let name = p.file_stem().unwrap().to_string_lossy().into_owned();
                (name, crate::ofn_read::parse(&text).unwrap().axioms)
            })
            .collect();
        let synthetic = [
            "SubClassOf(:A ObjectMinCardinality(2 :r))",
            "SubClassOf(:A ObjectMinCardinality(64 :r :B))",
            "SubClassOf(:A ObjectMinCardinality(65 :r :B))",
            "SubClassOf(:A ObjectMaxCardinality(1 :r :B))",
            "SubClassOf(:A ObjectMaxCardinality(2 :r :B))",
            "SubClassOf(:A ObjectExactCardinality(1 :r :B))",
            "SubClassOf(ObjectMinCardinality(2 :r :B) :A)",
            "SubClassOf(ObjectMaxCardinality(1 :r :B) :A)",
            "SubClassOf(ObjectSomeValuesFrom(:r ObjectMinCardinality(2 :s :B)) :C)",
            "SubClassOf(ObjectSomeValuesFrom(:r ObjectMaxCardinality(1 :s :B)) :C)",
            "SubClassOf(:A ObjectIntersectionOf(ObjectComplementOf(:B) ObjectMinCardinality(0 :r ObjectMinCardinality(2 :r))))",
            "SubClassOf(ObjectMinCardinality(0 :r ObjectMaxCardinality(1 :s :A)) ObjectHasSelf(:t))",
            "SubClassOf(:A ObjectMinCardinality(2 owl:bottomObjectProperty))",
            "SubClassOf(:A ObjectMaxCardinality(1 owl:topObjectProperty :B))",
            "FunctionalObjectProperty(:r)",
            "InverseFunctionalObjectProperty(:r)",
            "AsymmetricObjectProperty(:r)",
            "DisjointObjectProperties(:r :s)",
            "FunctionalObjectProperty(:r) TransitiveObjectProperty(:r)",
            "FunctionalObjectProperty(:r) SubObjectPropertyOf(ObjectPropertyChain(:s :s) :r)",
            "FunctionalObjectProperty(:r) EquivalentObjectProperties(:r :s)",
            "FunctionalObjectProperty(:r) InverseObjectProperties(:r :s) TransitiveObjectProperty(:s)",
            "DisjointClasses(ObjectMinCardinality(2 :r) ObjectMinCardinality(2 :r) :B)",
            "DisjointClasses(ObjectMaxCardinality(1 :r) :B)",
            "DisjointUnion(:C ObjectMinCardinality(2 :r) :D)",
            "EquivalentClasses(:A ObjectMinCardinality(2 :r :B))",
            "ObjectPropertyRange(:r ObjectMaxCardinality(1 :s))",
            "ObjectPropertyDomain(:r ObjectMinCardinality(2 :s))",
            "ClassAssertion(ObjectMinCardinality(2 :r) :a)",
            "SubClassOf(:A ObjectUnionOf(:B ObjectMinCardinality(2 :r)))",
            "SubClassOf(:A ObjectMinCardinality(2 :r ObjectUnionOf(:B :C)))",
            "SubClassOf(:A ObjectMinCardinality(64 :r owl:Nothing))",
            "SubClassOf(:A ObjectMinCardinality(65 :r owl:Nothing))",
            "SubClassOf(ObjectMaxCardinality(63 :r owl:Nothing) :A)",
            "SubClassOf(ObjectMaxCardinality(64 :r owl:Nothing) :A)",
            "SubClassOf(ObjectMinCardinality(65 :r owl:Nothing) :A)",
            "SubClassOf(:A ObjectMinCardinality(2 :r ObjectMinCardinality(65 :s owl:Nothing)))",
            "SubClassOf(:A ObjectIntersectionOf(ObjectMinCardinality(2 :s) ObjectUnionOf(ObjectSomeValuesFrom(:r :B) ObjectMinCardinality(1 :r :B))))",
            "SubClassOf(:A ObjectUnionOf(ObjectMaxCardinality(1 :r :B) ObjectMaxCardinality(1 :r ObjectIntersectionOf(:B owl:Thing))))",
            "SubClassOf(:A ObjectUnionOf(ObjectMaxCardinality(1 :r :B) ObjectMaxCardinality(1 :r :C)))",
            "ClassAssertion(ObjectMinCardinality(64 :r owl:Nothing) :a)",
            "ClassAssertion(ObjectMinCardinality(65 :r owl:Nothing) :a)",
            "ObjectPropertyRange(:r ObjectMinCardinality(65 :s owl:Nothing))",
            "ObjectPropertyDomain(:r ObjectMinCardinality(65 :s owl:Nothing))",
            "EquivalentClasses(:A ObjectMinCardinality(65 :r owl:Nothing))",
            "DisjointClasses(:B ObjectMaxCardinality(63 :r owl:Nothing))",
            "DisjointClasses(:B ObjectMaxCardinality(64 :r owl:Nothing))",
            "DisjointUnion(:C ObjectMinCardinality(65 :r owl:Nothing) :D)",
            "SubClassOf(:A ObjectComplementOf(ObjectMaxCardinality(64 :r owl:Nothing)))",
            "SubClassOf(:A ObjectExactCardinality(65 :r owl:Nothing))",
            "SubClassOf(ObjectExactCardinality(64 :r owl:Nothing) :A)",
            "SubClassOf(:A ObjectIntersectionOf(ObjectMinCardinality(2 :s) ObjectUnionOf(ObjectHasValue(:r :a) ObjectSomeValuesFrom(:r ObjectOneOf(:a)))))",
            "SubClassOf(:A ObjectIntersectionOf(ObjectMinCardinality(2 :s) ObjectUnionOf(ObjectMinCardinality(1 :r) ObjectSomeValuesFrom(:r owl:Thing))))",
            "SubClassOf(:A ObjectIntersectionOf(ObjectMinCardinality(2 :s) ObjectUnionOf(ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C)) ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C owl:Thing)))))",
            "SubClassOf(:A ObjectIntersectionOf(ObjectMinCardinality(2 :s) ObjectUnionOf(ObjectSomeValuesFrom(:r :B) ObjectSomeValuesFrom(:r ObjectUnionOf(:B owl:Nothing)))))",
            "SubClassOf(ObjectIntersectionOf(:A ObjectMinCardinality(2 :r :B)) ObjectMaxCardinality(1 :r :B))",
            "SubClassOf(ObjectIntersectionOf(:A ObjectMaxCardinality(0 :r ObjectMaxCardinality(1 :s :B))) ObjectSomeValuesFrom(:r ObjectMaxCardinality(1 :s :B)))",
            "FunctionalObjectProperty(:q) EquivalentObjectProperties(:p :q ObjectInverseOf(:s))",
            "FunctionalObjectProperty(:q) EquivalentObjectProperties(:p :q ObjectInverseOf(:s)) TransitiveObjectProperty(:s)",
        ];
        for (n, body) in synthetic.iter().enumerate() {
            let text = format!("Prefix(:=<urn:dl1286:>)\nOntology(\n{body}\n)");
            inputs.push((
                format!("synthetic-{n}"),
                crate::ofn_read::parse(&text).unwrap().axioms,
            ));
        }
        let (mut refused, mut fixtures, mut missed) = (0, Vec::new(), Vec::new());
        for (name, input) in &inputs {
            let (it, ax, outside) = built(input);
            let refs: Vec<&AxiomKind> = outside.iter().collect();
            let Some(i) = refusing(&refs, &it, Roles::El(&ax), &|| false).unwrap() else {
                // an axiom that only counting keeps from the rooted rules and that the Horn
                // rules take, which the check misses
                if let (Some(without), Some(with)) = (tier_d(input, false), tier_d(input, true)) {
                    let (mut it, mut ax, _) = built(input);
                    let horn =
                        super::super::horn::normalize(&refs, &mut it, &mut ax, None, &|| false)
                            .unwrap();
                    if let Some(j) = (0..refs.len()).find(|&j| !without[j] && with[j] && horn[j]) {
                        missed.push(format!("{name}: {:?}", refs[j]));
                    }
                }
                continue;
            };
            refused += 1;
            if !name.starts_with("synthetic") {
                fixtures.push(name.clone());
            }
            let at = format!("{name}: {:?}", refs[i]);
            if let Some(flags) = tier_d(input, false) {
                assert!(!flags[i], "{at}: the rooted rules without counting take it");
            }
            if let Some(flags) = tier_d(input, true) {
                assert!(
                    flags[i],
                    "{at}: the rooted rules with counting leave it out"
                );
            }
            let (mut it, mut ax, _) = built(input);
            let flags =
                super::super::horn::normalize(&refs, &mut it, &mut ax, None, &|| false).unwrap();
            assert!(flags[i], "{at}: the Horn rules leave it out");
        }
        for name in ["mixed-031", "sroiq-017", "sroiq-030", "sroiq-032"] {
            assert!(
                fixtures.iter().any(|f| f == name),
                "{name} does not refuse: {fixtures:?}"
            );
        }
        assert!(missed.is_empty(), "{missed:#?}");
        eprintln!("refused {refused} of {}: {fixtures:?}", inputs.len());
    }

    /// The check over item 805's rewriting, with the candidates the build reads: the
    /// rewritten axioms outside the profile, beside the original axioms. `None` when the
    /// rewriting is not needed.
    fn refuses_reduced(body: &str) -> Option<bool> {
        let text = format!("Prefix(:=<urn:dl1286:>)\nOntology(\n{body}\n)");
        let input = crate::ofn_read::parse(&text).unwrap().axioms;
        let original: Vec<&AxiomKind> = input.iter().map(|a| &a.kind).collect();
        let red = super::super::dl_reduce::plan(&original, None, None)?;
        let all = red.all(&original);
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        let mut candidates = Vec::new();
        for k in all {
            if super::super::logical(k) && super::super::el_break(k).is_some() {
                candidates.push(k);
            } else {
                let _ = super::super::index::normalize(k, &mut it, &mut ax);
            }
        }
        candidates.sort();
        candidates.dedup();
        Some(
            refuses(&candidates, &it, Roles::Original(&red, &original), &|| {
                false
            })
            .unwrap(),
        )
    }

    /// Over the rewriting, the rooted rules read the rewritten axioms and the Horn rules
    /// the original axioms each one stands for, with the role box of the original axioms.
    #[test]
    fn the_check_over_the_rewriting_asks_the_horn_rules_about_the_original_axioms() {
        // the two minimums and the stated axiom rewrite to one Horn axiom that needs
        // counting, and the Horn rules take the stated one
        assert_eq!(
            refuses_reduced(
                "DisjointClasses(
                     ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
                     ObjectMinCardinality(2 :r ObjectMinCardinality(1 owl:topObjectProperty :A)))
                 SubClassOf(ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
                     owl:Nothing)
                 ClassAssertion(:B :b)"
            ),
            Some(true)
        );
        // the same rewritten axiom standing for a disjoint union, which needs case analysis
        assert_eq!(
            refuses_reduced(
                "DisjointUnion(:C
                     ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
                     ObjectMinCardinality(2 :r ObjectMinCardinality(1 owl:topObjectProperty :A))
                     :D)
                 ClassAssertion(:B :b)"
            ),
            Some(false)
        );
        // a stated functional property beside a universal restriction over the universal
        // property, which the rewriting replaces, refuses while its property is simple in
        // the role box of the original axioms, and not once a transitive property is below
        // it there
        let universal = "SubClassOf(:B ObjectAllValuesFrom(owl:topObjectProperty :E))
                         SubClassOf(:E ObjectUnionOf(:F :G))
                         ClassAssertion(:B :b)
                         FunctionalObjectProperty(:p)";
        assert_eq!(refuses_reduced(universal), Some(true));
        assert_eq!(
            refuses_reduced(&format!(
                "{universal}
                 SubObjectPropertyOf(:s :p)
                 TransitiveObjectProperty(:s)"
            )),
            Some(false)
        );
        // a key, which the Horn rules do not take as an axiom outside OWL 2 EL
        assert_ne!(
            refuses_reduced(
                "HasKey(:K (:p) ())
                 SubClassOf(:K ObjectUnionOf(:E :F))
                 ClassAssertion(:K :k)"
            ),
            Some(true)
        );
    }
}
