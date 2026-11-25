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

#[inline]
fn class_iri_pl(c: &Class) -> Pl {
    match c.as_str() {
        vocab::OWL_THING | vocab::OWL_NOTHING => pl(0, 0),
        _ => pl(1, 0),
    }
}

/// The counts of a class expression, computed in O(1) auxiliary space with cooperative
/// polling at every class-expression and data-range node.
fn class_polled<E>(c: &ClassExpression, poll: &mut impl FnMut() -> Result<(), E>) -> Result<Pl, E> {
    use ClassExpression as C;
    poll()?;
    Ok(match c {
        C::Class(c) => class_iri_pl(c),
        C::ObjectIntersectionOf(cs) => {
            let mut pos = 0u8;
            let mut neg = 0u8;
            for elem in cs {
                let p = class_polled(elem, poll)?;
                pos = pos.max(p.pos);
                neg = add(neg, p.neg);
            }
            pl(pos, neg)
        }
        C::ObjectUnionOf(cs) => {
            let mut pos = 0u8;
            let mut neg = 0u8;
            for elem in cs {
                let p = class_polled(elem, poll)?;
                pos = add(pos, p.pos);
                neg = neg.max(p.neg);
            }
            pl(pos, neg)
        }
        C::ObjectComplementOf(c) => {
            let p = class_polled(c, poll)?;
            pl(p.neg, p.pos)
        }
        C::ObjectOneOf(is) => pl(is.len().min(2) as u8, 0),
        C::ObjectSomeValuesFrom(_, f) => {
            let p = class_polled(f, poll)?;
            pl(p.pos.max(1), p.neg)
        }
        C::ObjectAllValuesFrom(_, f) => {
            let p = class_polled(f, poll)?;
            pl(p.pos, p.neg.max(1))
        }
        C::ObjectHasValue(..) | C::ObjectHasSelf(_) | C::DataHasValue(..) => pl(1, 0),
        C::ObjectMinCardinality(n, _, f) => {
            let p = match f {
                Some(f) => class_polled(f, poll)?,
                None => pl(0, 0),
            };
            at_least(*n, p)
        }
        C::ObjectMaxCardinality(n, _, f) => {
            let p = match f {
                Some(f) => class_polled(f, poll)?,
                None => pl(0, 0),
            };
            at_most(*n, p)
        }
        C::ObjectExactCardinality(n, _, f) => {
            let p = match f {
                Some(f) => class_polled(f, poll)?,
                None => pl(0, 0),
            };
            both(at_least(*n, p), at_most(*n, p))
        }
        C::DataSomeValuesFrom(_, r) => {
            let p = data_polled(r, poll)?;
            pl(p.pos.max(1), p.neg)
        }
        C::DataAllValuesFrom(_, r) => {
            let p = data_polled(r, poll)?;
            pl(p.pos, p.neg.max(1))
        }
        C::DataMinCardinality(n, _, r) => {
            let p = match r {
                Some(r) => data_polled(r, poll)?,
                None => pl(1, 0),
            };
            at_least(*n, p)
        }
        C::DataMaxCardinality(n, _, r) => {
            let p = match r {
                Some(r) => data_polled(r, poll)?,
                None => pl(1, 0),
            };
            at_most(*n, p)
        }
        C::DataExactCardinality(n, _, r) => {
            let p = match r {
                Some(r) => data_polled(r, poll)?,
                None => pl(1, 0),
            };
            both(at_least(*n, p), at_most(*n, p))
        }
    })
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
/// union adds its operands', an enumeration counts its values. Computed in O(1) auxiliary
/// space with cooperative polling at every node.
fn data_polled<E>(r: &DataRange, poll: &mut impl FnMut() -> Result<(), E>) -> Result<Pl, E> {
    poll()?;
    Ok(match r {
        DataRange::Datatype(_) | DataRange::DatatypeRestriction(..) => pl(1, 0),
        DataRange::IntersectionOf(rs) => {
            let mut pos = 0u8;
            let mut neg = 0u8;
            for elem in rs {
                let p = data_polled(elem, poll)?;
                pos = pos.max(p.pos);
                neg = add(neg, p.neg);
            }
            pl(pos, neg)
        }
        DataRange::UnionOf(rs) => {
            let mut pos = 0u8;
            let mut neg = 0u8;
            for elem in rs {
                let p = data_polled(elem, poll)?;
                pos = add(pos, p.pos);
                neg = neg.max(p.neg);
            }
            pl(pos, neg)
        }
        DataRange::ComplementOf(r) => {
            let p = data_polled(r, poll)?;
            pl(p.neg, p.pos)
        }
        DataRange::OneOf(ls) => pl(ls.len().min(2) as u8, 0),
    })
}

/// Whether a class expression is empty when every class and property is.
fn bottom(c: &ClassExpression) -> bool {
    use ClassExpression as C;
    match c {
        C::Class(c) => c.as_str() != vocab::OWL_THING,
        C::ObjectIntersectionOf(cs) => cs.iter().any(bottom),
        C::ObjectUnionOf(cs) => cs.iter().all(bottom),
        C::ObjectComplementOf(c) => top(c),
        // the individuals of the signature are not emptied
        C::ObjectOneOf(_) => false,
        C::ObjectSomeValuesFrom(p, f) => !is_top_role(p) || bottom(f),
        C::ObjectHasValue(p, _) | C::ObjectHasSelf(p) => !is_top_role(p),
        C::ObjectMinCardinality(n, p, f) | C::ObjectExactCardinality(n, p, f) => {
            *n > 0 && (!is_top_role(p) || f.as_deref().is_some_and(bottom))
        }
        // the top property relates every element to every other, so `∀U.F` is empty
        // when F is
        C::ObjectAllValuesFrom(p, f) => is_top_role(p) && bottom(f),
        C::DataSomeValuesFrom(ps, _) => ps.iter().all(|d| !is_top_data(d)),
        C::DataHasValue(d, _) => !is_top_data(d),
        C::DataMinCardinality(n, d, _) | C::DataExactCardinality(n, d, _) => {
            *n > 0 && !is_top_data(d)
        }
        C::ObjectMaxCardinality(..) => false,
        C::DataAllValuesFrom(..) | C::DataMaxCardinality(..) => false,
    }
}

/// Whether a class expression is everything when every class and property is empty.
fn top(c: &ClassExpression) -> bool {
    use ClassExpression as C;
    match c {
        C::Class(c) => c.as_str() == vocab::OWL_THING,
        C::ObjectIntersectionOf(cs) => cs.iter().all(top),
        C::ObjectUnionOf(cs) => cs.iter().any(top),
        C::ObjectComplementOf(c) => bottom(c),
        C::ObjectAllValuesFrom(p, f) => !is_top_role(p) || top(f),
        C::ObjectMaxCardinality(_, p, f) | C::ObjectExactCardinality(0, p, f) => {
            !is_top_role(p) || f.as_deref().is_some_and(bottom)
        }
        C::ObjectMinCardinality(0, ..) | C::DataMinCardinality(0, ..) => true,
        C::DataAllValuesFrom(ps, _) => ps.iter().all(|d| !is_top_data(d)),
        C::DataMaxCardinality(_, d, _) | C::DataExactCardinality(0, d, _) => !is_top_data(d),
        _ => false,
    }
}

fn is_top_role(p: &ObjectPropertyExpression) -> bool {
    p.named().as_str() == vocab::OWL_TOP_OBJECT_PROPERTY
}

fn is_top_data(d: &DataProperty) -> bool {
    d.as_str() == vocab::OWL_TOP_DATA_PROPERTY
}

/// The largest number of disjuncts in any clause `ax` stands for (0, 1, or 2), computed in
/// O(1) auxiliary space with cooperative polling at every class-expression and data-range
/// operand. Axioms about properties alone return 0.
fn max_clause_polled<E>(ax: &AxiomKind, poll: &mut impl FnMut() -> Result<(), E>) -> Result<u8, E> {
    use AxiomKind as K;
    // C ⊑ D has ¬C ⊔ D as its clause
    let sub = |c: Pl, d: Pl| add(c.neg, d.pos);
    Ok(match ax {
        K::SubClassOf(c, d) => sub(class_polled(c, poll)?, class_polled(d, poll)?),
        K::EquivalentClasses(cs) => {
            // Maximum of `add(p_i.neg, p_j.pos)` over all ordered pairs `i != j`, computed
            // in one streaming pass using the running maximum positive and negative counts.
            let mut has_prev = false;
            let mut max_pos = 0u8;
            let mut max_neg = 0u8;
            let mut most = 0u8;
            for elem in cs {
                let p = class_polled(elem, poll)?;
                if has_prev {
                    most = most.max(add(p.neg, max_pos)).max(add(max_neg, p.pos));
                } else {
                    has_prev = true;
                }
                max_pos = max_pos.max(p.pos);
                max_neg = max_neg.max(p.neg);
            }
            most
        }
        K::DisjointClasses(cs) => {
            let (mut a, mut b) = (0u8, 0u8);
            for elem in cs {
                let n = class_polled(elem, poll)?.neg;
                if n > a {
                    b = a;
                    a = n;
                } else if n > b {
                    b = n;
                }
            }
            add(a, b)
        }
        K::DisjointUnion(c, cs) => {
            let mut union_pos = 0u8;
            let mut union_neg = 0u8;
            let (mut a, mut b) = (0u8, 0u8);
            for elem in cs {
                let p = class_polled(elem, poll)?;
                union_pos = add(union_pos, p.pos);
                union_neg = union_neg.max(p.neg);
                if p.neg > a {
                    b = a;
                    a = p.neg;
                } else if p.neg > b {
                    b = p.neg;
                }
            }
            let union = pl(union_pos, union_neg);
            let named = class_iri_pl(c);
            sub(named, union).max(sub(union, named)).max(add(a, b))
        }
        K::ClassAssertion(c, _)
        | K::ObjectPropertyDomain(_, c)
        | K::ObjectPropertyRange(_, c)
        | K::DataPropertyDomain(_, c) => class_polled(c, poll)?.pos,
        K::DataPropertyRange(_, r) => data_polled(r, poll)?.pos,
        K::DatatypeDefinition(_, r) => {
            let p = data_polled(r, poll)?;
            p.pos.max(add(p.neg, 1))
        }
        // C(x) ∧ C(y) ∧ the key's values shared → x = y, an equality besides the two
        // negative occurrences of C
        K::HasKey(c, ..) => {
            let n = class_polled(c, poll)?.neg;
            add(1, add(n, n))
        }
        _ => 0,
    })
}

/// Whether one of the clauses `ax` stands for is not ⊥-local for the empty signature.
fn is_global(ax: &AxiomKind) -> bool {
    use AxiomKind as K;
    match ax {
        K::SubClassOf(c, d) => !bottom(c) && !top(d),
        K::EquivalentClasses(cs) => !(cs.iter().all(bottom) || cs.iter().all(top)),
        K::DisjointClasses(cs) => cs.iter().filter(|c| !bottom(c)).count() > 1,
        K::DisjointUnion(c, cs) => {
            // c ≡ ⊔cs is local when both sides are emptied or both are everything, and
            // the disjointness of the parts when at most one of them is not emptied
            let named_bottom = c.as_str() != vocab::OWL_THING;
            let named_top = c.as_str() == vocab::OWL_THING;
            let equal =
                (named_bottom && cs.iter().all(bottom)) || (named_top && cs.iter().any(top));
            let apart = cs.iter().filter(|c| !bottom(c)).count() <= 1;
            !(equal && apart)
        }
        K::ClassAssertion(c, _) => !top(c),
        K::ObjectPropertyDomain(p, c) | K::ObjectPropertyRange(p, c) => is_top_role(p) && !top(c),
        K::DataPropertyDomain(d, c) => is_top_data(d) && !top(c),
        // a data range is not emptied with the signature, so a range of the top data
        // property is global unless it holds of every value
        K::DataPropertyRange(d, r) => is_top_data(d) && !all_values(r),
        _ => false,
    }
}

/// Whether a data range holds of every data value.
fn all_values(r: &DataRange) -> bool {
    match r {
        DataRange::Datatype(d) => d.as_str() == vocab::RDFS_LITERAL,
        DataRange::UnionOf(rs) => rs.iter().any(all_values),
        DataRange::IntersectionOf(rs) => rs.iter().all(all_values),
        _ => false,
    }
}

/// Streaming, allocation-free, cooperatively polled Horn check matching `shape(ax).horn`.
pub fn is_horn_polled<E>(
    ax: &AxiomKind,
    poll: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    Ok(max_clause_polled(ax, poll)? <= 1)
}

/// What an axiom needs: whether it is Horn and, when it is not, whether it is global.
pub fn shape(ax: &AxiomKind) -> Shape {
    let Ok(most) = max_clause_polled(ax, &mut || Ok::<(), std::convert::Infallible>(()));
    let horn = most <= 1;
    Shape {
        horn,
        global: !horn && is_global(ax),
    }
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

    #[test]
    fn streaming_is_horn_polled_matches_shape_and_stops_on_poll_error() {
        let cases = [
            "SubClassOf(:A ObjectUnionOf(:B :C))",
            "SubClassOf(ObjectUnionOf(:B :C) :A)",
            "EquivalentClasses(:A :B)",
            "EquivalentClasses(:A ObjectUnionOf(:B :C))",
            "EquivalentClasses(:A ObjectComplementOf(:B))",
            "EquivalentClasses(:A ObjectAllValuesFrom(:r :B) :C)",
            "DisjointClasses(:A :B :C)",
            "DisjointClasses(ObjectComplementOf(:A) ObjectComplementOf(:B))",
            "DisjointUnion(:A :B :C)",
            "DisjointUnion(:A owl:Nothing :B)",
            "HasKey(:A (:r) ())",
            "HasKey(ObjectComplementOf(:A) (:r) ())",
            "DataPropertyRange(:d DataUnionOf(xsd:int xsd:string))",
            "DataPropertyRange(:d DataIntersectionOf(xsd:int xsd:string))",
        ];
        for src in cases {
            let doc = ofn_read::parse(&format!(
                "Prefix(:=<http://ex.org/>)\nPrefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
                 Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{src}\n)"
            ))
            .unwrap();
            let ax = doc
                .axioms
                .iter()
                .map(|a| &a.kind)
                .find(|k| !matches!(k, AxiomKind::Declaration(_)))
                .unwrap();
            let polled = is_horn_polled(ax, &mut || Ok::<(), ()>(())).unwrap();
            assert_eq!(polled, shape(ax).horn, "{src}");
        }

        // Cooperative poll error halts mid-expression.
        let wide = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("http://ex.org/A")),
            ClassExpression::ObjectUnionOf(
                (0..100usize)
                    .map(|i| ClassExpression::Class(Class::new(format!("http://ex.org/C{i}"))))
                    .collect(),
            ),
        );
        let mut ticks = 0usize;
        let res = is_horn_polled(&wide, &mut || {
            ticks += 1;
            if ticks == 10 { Err(()) } else { Ok(()) }
        });
        assert_eq!(res, Err(()));
        assert_eq!(ticks, 10);
    }
}
