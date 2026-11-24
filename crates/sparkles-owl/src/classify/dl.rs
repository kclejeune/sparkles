//! What the axioms outside OWL 2 EL need of the classifier (roadmap item 603): whether
//! each is Horn, so that the deterministic rules of the Horn description logics suffice
//! for it, or needs case analysis, and whether a non-Horn axiom is global, so that its
//! premise holds in every context.
//!
//! An axiom is Horn when the clause it stands for has at most one disjunct. The polarity
//! counts of Krötzsch, Rudolph and Hitzler (*Complexities of Horn Description Logics*, ACM
//! TOCL 2013) give the number: for a class expression, how many disjuncts it adds to the
//! clause in a positive position and in a negative one. A disjunction or an enumeration of
//! several individuals in a positive position, a complement or a universal restriction in a
//! negative position, a maximum cardinality above one in a positive position and a minimum
//! above one in a negative position each add one. Data ranges count the same way. A finite
//! value space is a disjunction that the syntax does not show, which the datatype rules of
//! a later phase count.
//!
//! An axiom is global when it is not ⊥-local for the empty signature (Cuenca Grau,
//! Horrocks, Kazakov and Sattler, *Modular Reuse of Ontologies*, JAIR 2008): it does not
//! become a tautology when every class and property is empty, so its premise holds
//! everywhere, as in `⊤ ⊑ A ⊔ B` or `¬A ⊑ B`.

use crate::model::*;
use crate::vocab;

/// The rules a run needs, from the cheapest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// every axiom is inside OWL 2 EL
    #[default]
    El,
    /// every axiom is Horn
    Horn,
    /// some axiom needs case analysis
    Disjunctive,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::El => "el",
            Tier::Horn => "horn",
            Tier::Disjunctive => "disjunctive",
        }
    }
}

/// What an axiom outside OWL 2 EL needs: whether it is Horn, and, when it is not,
/// whether it is global.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub horn: bool,
    pub global: bool,
}

/// The counts of disjuncts a class expression or a data range adds to a clause, in a
/// positive position and in a negative one, capped at two, which is all that matters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pl {
    pos: u8,
    neg: u8,
}

const fn pl(pos: u8, neg: u8) -> Pl {
    Pl {
        pos: if pos > 2 { 2 } else { pos },
        neg: if neg > 2 { 2 } else { neg },
    }
}

fn add(a: u8, b: u8) -> u8 {
    (a + b).min(2)
}

struct PollState<'a, E> {
    step: usize,
    visited: usize,
    poll: &'a mut dyn FnMut() -> Result<(), E>,
}

impl<E> PollState<'_, E> {
    #[inline]
    fn tick(&mut self) -> Result<(), E> {
        self.visited += 1;
        if self.visited.is_multiple_of(self.step) {
            (self.poll)()?;
        }
        Ok(())
    }
}

/// The counts of a class expression, computed without heap allocation and polling
/// cooperatively every `state.step` nodes.
fn class<E>(c: &ClassExpression, state: &mut PollState<'_, E>) -> Result<Pl, E> {
    use ClassExpression as C;
    state.tick()?;
    Ok(match c {
        C::Class(c) => class_name_pl(c),
        C::ObjectIntersectionOf(cs) => {
            let (mut max_pos, mut sum_neg) = (0u8, 0u8);
            for c in cs {
                let p = class(c, state)?;
                max_pos = max_pos.max(p.pos);
                sum_neg = add(sum_neg, p.neg);
            }
            pl(max_pos, sum_neg)
        }
        C::ObjectUnionOf(cs) => {
            let (mut sum_pos, mut max_neg) = (0u8, 0u8);
            for c in cs {
                let p = class(c, state)?;
                sum_pos = add(sum_pos, p.pos);
                max_neg = max_neg.max(p.neg);
            }
            pl(sum_pos, max_neg)
        }
        C::ObjectComplementOf(c) => {
            let p = class(c, state)?;
            pl(p.neg, p.pos)
        }
        C::ObjectOneOf(is) => pl(is.len().min(2) as u8, 0),
        C::ObjectSomeValuesFrom(_, f) => {
            let p = class(f, state)?;
            pl(p.pos.max(1), p.neg)
        }
        C::ObjectAllValuesFrom(_, f) => {
            let p = class(f, state)?;
            pl(p.pos, p.neg.max(1))
        }
        C::ObjectHasValue(..) | C::ObjectHasSelf(_) | C::DataHasValue(..) => pl(1, 0),
        C::ObjectMinCardinality(n, _, f) => {
            let p = match f.as_deref() {
                Some(f) => class(f, state)?,
                None => pl(0, 0),
            };
            at_least(*n, p)
        }
        C::ObjectMaxCardinality(n, _, f) => {
            let p = match f.as_deref() {
                Some(f) => class(f, state)?,
                None => pl(0, 0),
            };
            at_most(*n, p)
        }
        C::ObjectExactCardinality(n, _, f) => {
            let p = match f.as_deref() {
                Some(f) => class(f, state)?,
                None => pl(0, 0),
            };
            both(at_least(*n, p), at_most(*n, p))
        }
        C::DataSomeValuesFrom(_, r) => {
            let p = data(r, state)?;
            pl(p.pos.max(1), p.neg)
        }
        C::DataAllValuesFrom(_, r) => {
            let p = data(r, state)?;
            pl(p.pos, p.neg.max(1))
        }
        C::DataMinCardinality(n, _, r) => {
            let p = match r.as_ref() {
                Some(r) => data(r, state)?,
                None => pl(1, 0),
            };
            at_least(*n, p)
        }
        C::DataMaxCardinality(n, _, r) => {
            let p = match r.as_ref() {
                Some(r) => data(r, state)?,
                None => pl(1, 0),
            };
            at_most(*n, p)
        }
        C::DataExactCardinality(n, _, r) => {
            let p = match r.as_ref() {
                Some(r) => data(r, state)?,
                None => pl(1, 0),
            };
            both(at_least(*n, p), at_most(*n, p))
        }
    })
}

fn class_name_pl(c: &Class) -> Pl {
    match c.as_str() {
        vocab::OWL_THING | vocab::OWL_NOTHING => pl(0, 0),
        _ => pl(1, 0),
    }
}

/// `≥n R.F`, F's counts being `f`: an existential in a positive position, and in a
/// negative position the dual `≤n-1 R.F` in a positive one.
fn at_least(n: u32, f: Pl) -> Pl {
    if n == 0 {
        return pl(0, 0);
    }
    pl(f.pos.max(1), at_most(n - 1, f).pos)
}

/// `≤n R.F`, the negation of `≥n+1 R.F`, whose filler keeps its position there: in a
/// positive position it is `∀R.¬F` for n zero, one disjunct more than F's negative count
/// for n one, and a disjunction for n above one; in a negative position it is the
/// existential `≥n+1 R.F`.
fn at_most(n: u32, f: Pl) -> Pl {
    let pos = match n {
        0 => f.neg,
        1 => add(1, f.neg),
        _ => 2,
    };
    pl(pos, f.pos.max(1))
}

/// An intersection of two counts.
fn both(a: Pl, b: Pl) -> Pl {
    pl(a.pos.max(b.pos), add(a.neg, b.neg))
}

/// The counts of a data range: a datatype, a value or a restriction is one disjunct, a
/// union adds its operands', an enumeration counts its values.
fn data<E>(r: &DataRange, state: &mut PollState<'_, E>) -> Result<Pl, E> {
    state.tick()?;
    Ok(match r {
        DataRange::Datatype(_) | DataRange::DatatypeRestriction(..) => pl(1, 0),
        DataRange::IntersectionOf(rs) => {
            let (mut max_pos, mut sum_neg) = (0u8, 0u8);
            for r in rs {
                let p = data(r, state)?;
                max_pos = max_pos.max(p.pos);
                sum_neg = add(sum_neg, p.neg);
            }
            pl(max_pos, sum_neg)
        }
        DataRange::UnionOf(rs) => {
            let (mut sum_pos, mut max_neg) = (0u8, 0u8);
            for r in rs {
                let p = data(r, state)?;
                sum_pos = add(sum_pos, p.pos);
                max_neg = max_neg.max(p.neg);
            }
            pl(sum_pos, max_neg)
        }
        DataRange::ComplementOf(r) => {
            let p = data(r, state)?;
            pl(p.neg, p.pos)
        }
        DataRange::OneOf(ls) => pl(ls.len().min(2) as u8, 0),
    })
}

/// Whether a class expression is empty when every class and property is.
fn bottom<E>(c: &ClassExpression, state: &mut PollState<'_, E>) -> Result<bool, E> {
    use ClassExpression as C;
    state.tick()?;
    Ok(match c {
        C::Class(c) => c.as_str() != vocab::OWL_THING,
        C::ObjectIntersectionOf(cs) => {
            for c in cs {
                if bottom(c, state)? {
                    return Ok(true);
                }
            }
            false
        }
        C::ObjectUnionOf(cs) => {
            for c in cs {
                if !bottom(c, state)? {
                    return Ok(false);
                }
            }
            true
        }
        C::ObjectComplementOf(c) => top(c, state)?,
        // the individuals of the signature are not emptied
        C::ObjectOneOf(_) => false,
        C::ObjectSomeValuesFrom(p, f) => !is_top_role(p) || bottom(f, state)?,
        C::ObjectHasValue(p, _) | C::ObjectHasSelf(p) => !is_top_role(p),
        C::ObjectMinCardinality(n, p, f) | C::ObjectExactCardinality(n, p, f) => {
            *n > 0
                && (!is_top_role(p)
                    || match f.as_deref() {
                        Some(f) => bottom(f, state)?,
                        None => false,
                    })
        }
        // the top property relates every element to every other, so `∀U.F` is empty
        // when F is
        C::ObjectAllValuesFrom(p, f) => is_top_role(p) && bottom(f, state)?,
        C::DataSomeValuesFrom(ps, _) => ps.iter().all(|d| !is_top_data(d)),
        C::DataHasValue(d, _) => !is_top_data(d),
        C::DataMinCardinality(n, d, _) | C::DataExactCardinality(n, d, _) => {
            *n > 0 && !is_top_data(d)
        }
        C::ObjectMaxCardinality(..) => false,
        C::DataAllValuesFrom(..) | C::DataMaxCardinality(..) => false,
    })
}

/// Whether a class expression is everything when every class and property is empty.
fn top<E>(c: &ClassExpression, state: &mut PollState<'_, E>) -> Result<bool, E> {
    use ClassExpression as C;
    state.tick()?;
    Ok(match c {
        C::Class(c) => c.as_str() == vocab::OWL_THING,
        C::ObjectIntersectionOf(cs) => {
            for c in cs {
                if !top(c, state)? {
                    return Ok(false);
                }
            }
            true
        }
        C::ObjectUnionOf(cs) => {
            for c in cs {
                if top(c, state)? {
                    return Ok(true);
                }
            }
            false
        }
        C::ObjectComplementOf(c) => bottom(c, state)?,
        C::ObjectAllValuesFrom(p, f) => !is_top_role(p) || top(f, state)?,
        C::ObjectMaxCardinality(_, p, f) | C::ObjectExactCardinality(0, p, f) => {
            !is_top_role(p)
                || match f.as_deref() {
                    Some(f) => bottom(f, state)?,
                    None => false,
                }
        }
        C::ObjectMinCardinality(0, ..) | C::DataMinCardinality(0, ..) => true,
        C::DataAllValuesFrom(ps, _) => ps.iter().all(|d| !is_top_data(d)),
        C::DataMaxCardinality(_, d, _) | C::DataExactCardinality(0, d, _) => !is_top_data(d),
        _ => false,
    })
}

fn is_top_role(p: &ObjectPropertyExpression) -> bool {
    p.named().as_str() == vocab::OWL_TOP_OBJECT_PROPERTY
}

fn is_top_data(d: &DataProperty) -> bool {
    d.as_str() == vocab::OWL_TOP_DATA_PROPERTY
}

/// The counts of the clauses an axiom stands for, as the largest number of disjuncts of
/// one of them, and whether one of them is not ⊥-local for the empty signature. Axioms
/// about properties alone are Horn and local.
fn clauses<E>(ax: &AxiomKind, state: &mut PollState<'_, E>) -> Result<(u8, bool), E> {
    use AxiomKind as K;
    state.tick()?;
    // C ⊑ D has ¬C ⊔ D as its clause
    let sub = |c: Pl, d: Pl| add(c.neg, d.pos);
    Ok(match ax {
        K::SubClassOf(c, d) => {
            let most = sub(class(c, state)?, class(d, state)?);
            let global = most > 1 && !bottom(c, state)? && !top(d, state)?;
            (most, global)
        }
        K::EquivalentClasses(cs) => {
            let mut best_pos: [(u8, usize); 2] = [(0, usize::MAX); 2];
            let mut best_neg: [(u8, usize); 2] = [(0, usize::MAX); 2];
            for (i, c) in cs.iter().enumerate() {
                let p = class(c, state)?;
                if best_pos[0].1 == usize::MAX || p.pos > best_pos[0].0 {
                    best_pos = [(p.pos, i), best_pos[0]];
                } else if best_pos[1].1 == usize::MAX || p.pos > best_pos[1].0 {
                    best_pos[1] = (p.pos, i);
                }
                if best_neg[0].1 == usize::MAX || p.neg > best_neg[0].0 {
                    best_neg = [(p.neg, i), best_neg[0]];
                } else if best_neg[1].1 == usize::MAX || p.neg > best_neg[1].0 {
                    best_neg[1] = (p.neg, i);
                }
            }
            let mut most = 0u8;
            for &(neg, i) in &best_neg {
                if i == usize::MAX {
                    continue;
                }
                for &(pos, j) in &best_pos {
                    if j != usize::MAX && i != j {
                        most = most.max(add(neg, pos));
                    }
                }
            }
            let global = if most > 1 {
                let mut all_bot = true;
                for c in cs {
                    if !bottom(c, state)? {
                        all_bot = false;
                        break;
                    }
                }
                let mut all_top = true;
                if !all_bot {
                    for c in cs {
                        if !top(c, state)? {
                            all_top = false;
                            break;
                        }
                    }
                }
                !(all_bot || all_top)
            } else {
                false
            };
            (most, global)
        }
        K::DisjointClasses(cs) => {
            let (mut a, mut b) = (0u8, 0u8);
            for c in cs {
                let n = class(c, state)?.neg;
                if n > a {
                    b = a;
                    a = n;
                } else if n > b {
                    b = n;
                }
            }
            let most = add(a, b);
            let global = if most > 1 {
                let mut non_bot = 0usize;
                for c in cs {
                    if !bottom(c, state)? {
                        non_bot += 1;
                        if non_bot > 1 {
                            break;
                        }
                    }
                }
                non_bot > 1
            } else {
                false
            };
            (most, global)
        }
        K::DisjointUnion(c, cs) => {
            let (mut sum_pos, mut max_neg) = (0u8, 0u8);
            let (mut a, mut b) = (0u8, 0u8);
            for part in cs {
                let p = class(part, state)?;
                sum_pos = add(sum_pos, p.pos);
                max_neg = max_neg.max(p.neg);
                if p.neg > a {
                    b = a;
                    a = p.neg;
                } else if p.neg > b {
                    b = p.neg;
                }
            }
            let union = pl(sum_pos, max_neg);
            let named = class_name_pl(c);
            let most = sub(named, union).max(sub(union, named)).max(add(a, b));
            // c ≡ ⊔cs is local when both sides are emptied or both are everything, and
            // the disjointness of the parts when at most one of them is not emptied
            let global = if most > 1 {
                let named_bot = c.as_str() != vocab::OWL_THING;
                let named_top = c.as_str() == vocab::OWL_THING;
                let mut non_bot = 0usize;
                for part in cs {
                    if !bottom(part, state)? {
                        non_bot += 1;
                    }
                }
                let all_bot = non_bot == 0;
                let any_top = if named_top {
                    let mut found = false;
                    for part in cs {
                        if top(part, state)? {
                            found = true;
                            break;
                        }
                    }
                    found
                } else {
                    false
                };
                let equal = (named_bot && all_bot) || (named_top && any_top);
                let apart = non_bot <= 1;
                !(equal && apart)
            } else {
                false
            };
            (most, global)
        }
        K::ClassAssertion(c, _) => {
            let most = class(c, state)?.pos;
            let global = most > 1 && !top(c, state)?;
            (most, global)
        }
        K::ObjectPropertyDomain(p, c) => {
            let most = class(c, state)?.pos;
            let global = most > 1 && is_top_role(p) && !top(c, state)?;
            (most, global)
        }
        K::ObjectPropertyRange(p, c) => {
            let most = class(c, state)?.pos;
            let global = most > 1 && is_top_role(p) && !top(c, state)?;
            (most, global)
        }
        K::DataPropertyDomain(d, c) => {
            let most = class(c, state)?.pos;
            let global = most > 1 && is_top_data(d) && !top(c, state)?;
            (most, global)
        }
        // a data range is not emptied with the signature, so a range of the top data
        // property is global unless it holds of every value
        K::DataPropertyRange(d, r) => {
            let most = data(r, state)?.pos;
            let global = most > 1 && is_top_data(d) && !all_values(r, state)?;
            (most, global)
        }
        K::DatatypeDefinition(_, r) => {
            let p = data(r, state)?;
            (p.pos.max(add(p.neg, 1)), false)
        }
        // C(x) ∧ C(y) ∧ the key's values shared → x = y, an equality besides the two
        // negative occurrences of C
        K::HasKey(c, ..) => {
            let n = class(c, state)?.neg;
            (add(1, add(n, n)), false)
        }
        _ => (0, false),
    })
}

/// Whether a data range holds of every data value.
fn all_values<E>(r: &DataRange, state: &mut PollState<'_, E>) -> Result<bool, E> {
    state.tick()?;
    Ok(match r {
        DataRange::Datatype(d) => d.as_str() == vocab::RDFS_LITERAL,
        DataRange::UnionOf(rs) => {
            for r in rs {
                if all_values(r, state)? {
                    return Ok(true);
                }
            }
            false
        }
        DataRange::IntersectionOf(rs) => {
            for r in rs {
                if !all_values(r, state)? {
                    return Ok(false);
                }
            }
            true
        }
        _ => false,
    })
}

/// What an axiom needs: whether it is Horn and, when it is not, whether it is global,
/// computed without heap allocation and polling `poll` every `poll_step` nodes.
pub(super) fn shape_polled<E>(
    ax: &AxiomKind,
    poll_step: usize,
    poll: &mut impl FnMut() -> Result<(), E>,
) -> Result<Shape, E> {
    let mut state = PollState {
        step: poll_step.max(1),
        visited: 0,
        poll,
    };
    let (most, global) = clauses(ax, &mut state)?;
    let horn = most <= 1;
    Ok(Shape {
        horn,
        global: !horn && global,
    })
}

/// What an axiom needs: whether it is Horn and, when it is not, whether it is global.
pub fn shape(ax: &AxiomKind) -> Shape {
    shape_polled(ax, usize::MAX, &mut || Ok::<(), ()>(())).unwrap()
}

/// The axioms outside OWL 2 EL of a run, counted by what they need.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutsideEl {
    /// Horn axioms outside OWL 2 EL
    pub horn: usize,
    /// axioms that need case analysis
    pub disjunctive: usize,
    /// the disjunctive axioms whose premise holds everywhere
    pub global: usize,
    /// the axioms the Horn rules take, which the run does not leave out
    pub taken: usize,
}

impl OutsideEl {
    pub fn add(&mut self, s: Shape) {
        if s.horn {
            self.horn += 1;
        } else {
            self.disjunctive += 1;
            if s.global {
                self.global += 1;
            }
        }
    }

    /// The tier these axioms need.
    pub fn tier(&self) -> Tier {
        if self.disjunctive > 0 {
            Tier::Disjunctive
        } else if self.horn > 0 {
            Tier::Horn
        } else {
            Tier::El
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ofn_read;

    /// The shape of the one logical axiom of a functional-style document.
    fn of(axiom: &str) -> Shape {
        let doc = ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nPrefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
             Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{axiom}\n)"
        ))
        .unwrap();
        let ax: Vec<&AxiomKind> = doc
            .axioms
            .iter()
            .map(|a| &a.kind)
            .filter(|k| !matches!(k, AxiomKind::Declaration(_)))
            .collect();
        assert_eq!(ax.len(), 1, "{axiom}");
        shape(ax[0])
    }

    fn horn(axiom: &str) -> bool {
        of(axiom).horn
    }

    #[test]
    fn disjunctions_count_by_their_position() {
        assert!(!horn("SubClassOf(:A ObjectUnionOf(:B :C))"));
        assert!(horn("SubClassOf(ObjectUnionOf(:B :C) :A)"));
        assert!(!horn(
            "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectUnionOf(:B :C)))"
        ));
        assert!(horn(
            "SubClassOf(ObjectSomeValuesFrom(:r ObjectUnionOf(:B :C)) :A)"
        ));
        assert!(!horn(
            "SubClassOf(:A ObjectUnionOf(ObjectSomeValuesFrom(:r :B) :C))"
        ));
        assert!(!horn("EquivalentClasses(:A ObjectUnionOf(:B :C))"));
        assert!(!horn("DisjointUnion(:A :B :C)"));
        assert!(!horn("ClassAssertion(ObjectOneOf(:a :b) :c)"));
        assert!(horn("SubClassOf(ObjectOneOf(:a :b) :C)"));
        assert!(horn("SubClassOf(:A ObjectOneOf(:a))"));
    }

    #[test]
    fn a_complement_on_the_left_is_a_disjunction() {
        assert!(!horn("SubClassOf(ObjectComplementOf(:A) :B)"));
        assert!(horn("SubClassOf(:A ObjectComplementOf(:B))"));
        assert!(horn("DisjointClasses(:A :B)"));
        assert!(horn("DisjointClasses(ObjectComplementOf(:A) :B)"));
        assert!(!horn(
            "DisjointClasses(ObjectComplementOf(:A) ObjectComplementOf(:B))"
        ));
        assert!(horn(
            "SubClassOf(:A ObjectComplementOf(ObjectIntersectionOf(:B :C)))"
        ));
        assert!(!horn(
            "SubClassOf(:A ObjectComplementOf(ObjectIntersectionOf(ObjectComplementOf(:B) ObjectComplementOf(:C))))"
        ));
    }

    #[test]
    fn universals_and_counting_count_by_their_position() {
        assert!(horn("SubClassOf(:A ObjectAllValuesFrom(:r :B))"));
        assert!(!horn("SubClassOf(ObjectAllValuesFrom(:r :B) :A)"));
        assert!(horn("SubClassOf(ObjectAllValuesFrom(:r :B) owl:Nothing)"));
        assert!(horn("SubClassOf(:A ObjectMaxCardinality(1 :r :B))"));
        assert!(horn("SubClassOf(:A ObjectMaxCardinality(0 :r :B))"));
        assert!(!horn("SubClassOf(:A ObjectMaxCardinality(2 :r :B))"));
        assert!(!horn(
            "SubClassOf(:A ObjectMaxCardinality(1 :r ObjectAllValuesFrom(:s :B)))"
        ));
        assert!(!horn("SubClassOf(ObjectMaxCardinality(1 :r :B) :A)"));
        assert!(horn("SubClassOf(:A ObjectMinCardinality(3 :r :B))"));
        assert!(!horn("SubClassOf(ObjectMinCardinality(2 :r :B) :A)"));
        assert!(horn(
            "SubClassOf(ObjectMinCardinality(2 :r :B) owl:Nothing)"
        ));
        assert!(horn("SubClassOf(ObjectMinCardinality(1 :r :B) :A)"));
        assert!(!horn("SubClassOf(:A ObjectExactCardinality(2 :r :B))"));
        assert!(horn("SubClassOf(:A ObjectExactCardinality(1 :r :B))"));
        assert!(horn("SubClassOf(:A ObjectHasValue(:r :a))"));
        assert!(horn("FunctionalObjectProperty(:r)"));
        assert!(horn("ObjectPropertyRange(:r ObjectAllValuesFrom(:s :B))"));
        assert!(!horn("ObjectPropertyRange(:r ObjectUnionOf(:B :C))"));
        assert!(horn("HasKey(:C (:r) ())"));
    }

    #[test]
    fn data_ranges_count_like_classes() {
        assert!(horn("SubClassOf(:A DataAllValuesFrom(:d xsd:integer))"));
        assert!(!horn("SubClassOf(DataAllValuesFrom(:d xsd:integer) :A)"));
        assert!(horn("SubClassOf(:A DataMinCardinality(2 :d xsd:boolean))"));
        assert!(!horn("SubClassOf(DataMinCardinality(2 :d xsd:boolean) :A)"));
        assert!(!horn(
            "SubClassOf(:A DataSomeValuesFrom(:d DataUnionOf(xsd:int xsd:string)))"
        ));
        assert!(!horn(
            "SubClassOf(:A DataSomeValuesFrom(:d DataOneOf(\"1\"^^xsd:integer \"2\"^^xsd:integer)))"
        ));
        assert!(!horn(
            "DataPropertyRange(:d DataUnionOf(xsd:int xsd:string))"
        ));
        assert!(horn("DataPropertyRange(:d DataComplementOf(xsd:int))"));
        assert!(!horn("SubClassOf(:A DataMaxCardinality(2 :d))"));
    }

    #[test]
    fn global_axioms_hold_where_every_name_is_empty() {
        let global = |a: &str| of(a).global;
        assert!(global("SubClassOf(owl:Thing ObjectUnionOf(:A :B))"));
        assert!(global("SubClassOf(ObjectComplementOf(:A) :B)"));
        assert!(global("SubClassOf(ObjectAllValuesFrom(:r :A) :B)"));
        assert!(global("SubClassOf(ObjectMaxCardinality(1 :r :A) :B)"));
        assert!(global("DisjointUnion(owl:Thing :A :B)"));
        assert!(global("ClassAssertion(ObjectUnionOf(:A :B) :a)"));
        assert!(!global("SubClassOf(:A ObjectUnionOf(:B :C))"));
        assert!(!global(
            "SubClassOf(ObjectSomeValuesFrom(:r :A) ObjectUnionOf(:B :C))"
        ));
        assert!(!global("EquivalentClasses(:A ObjectUnionOf(:B :C))"));
        assert!(!global("DisjointUnion(:A :B :C)"));
        // Horn axioms are never reported as global
        assert!(!global("SubClassOf(owl:Thing :A)"));
        // the top properties are not emptied
        assert!(global(
            "ObjectPropertyRange(owl:topObjectProperty ObjectUnionOf(:A :B))"
        ));
        assert!(!global("ObjectPropertyRange(:r ObjectUnionOf(:A :B))"));
        assert!(global(
            "DataPropertyRange(owl:topDataProperty DataUnionOf(xsd:integer xsd:string))"
        ));
        // a restriction is emptied when its filler is
        assert!(!global(
            "SubClassOf(ObjectSomeValuesFrom(owl:topObjectProperty owl:Nothing) ObjectUnionOf(:A :B))"
        ));
        assert!(!global(
            "SubClassOf(ObjectMinCardinality(2 owl:topObjectProperty owl:Nothing) ObjectUnionOf(:A :B))"
        ));
        assert!(!global(
            "SubClassOf(owl:Thing ObjectUnionOf(:A ObjectAllValuesFrom(owl:topObjectProperty owl:Thing)))"
        ));
        assert!(!global(
            "SubClassOf(owl:Thing ObjectUnionOf(:A ObjectMinCardinality(0 :r :B)))"
        ));
        // both sides of a disjoint union are everything and its parts but one are emptied
        assert!(!global(
            "DisjointUnion(owl:Thing ObjectComplementOf(:A) ObjectUnionOf(:B :C))"
        ));
    }

    #[test]
    fn minimums_in_a_negative_position_count_as_their_dual_maximum() {
        assert!(!horn(
            "SubClassOf(ObjectMinCardinality(3 :r :B) owl:Nothing)"
        ));
        assert!(!horn(
            "SubClassOf(ObjectComplementOf(ObjectMaxCardinality(2 :r :B)) owl:Nothing)"
        ));
        // ≥2 r.B ⊑ C is ≤1 r.B ⊔ C, an equality and C
        assert!(!horn("SubClassOf(ObjectMinCardinality(2 :r :B) :C)"));
        assert!(horn(
            "SubClassOf(ObjectMinCardinality(2 :r :B) owl:Nothing)"
        ));
        assert!(!horn(
            "SubClassOf(ObjectMinCardinality(2 :r ObjectComplementOf(:B)) owl:Nothing)"
        ));
        assert!(horn(
            "SubClassOf(ObjectMinCardinality(0 :r ObjectComplementOf(:B)) :C)"
        ));
    }

    #[test]
    fn a_key_concludes_an_equality() {
        assert!(horn("HasKey(:A (:r) ())"));
        assert!(!horn("HasKey(ObjectComplementOf(:A) (:r) ())"));
    }

    #[test]
    fn the_tier_is_the_most_that_one_axiom_needs() {
        let mut c = OutsideEl::default();
        assert_eq!(c.tier(), Tier::El);
        c.add(of("SubClassOf(:A ObjectAllValuesFrom(:r :B))"));
        assert_eq!(c.tier(), Tier::Horn);
        c.add(of("SubClassOf(owl:Thing ObjectUnionOf(:A :B))"));
        assert_eq!(c.tier(), Tier::Disjunctive);
        assert_eq!(
            c,
            OutsideEl {
                horn: 1,
                disjunctive: 1,
                global: 1,
                taken: 0
            }
        );
    }
}
