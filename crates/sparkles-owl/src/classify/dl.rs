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

fn sum(xs: impl Iterator<Item = u8>) -> u8 {
    xs.fold(0, add)
}

/// The counts of a class expression.
fn class(c: &ClassExpression) -> Pl {
    use ClassExpression as C;
    match c {
        C::Class(c) => match c.as_str() {
            vocab::OWL_THING | vocab::OWL_NOTHING => pl(0, 0),
            _ => pl(1, 0),
        },
        C::ObjectIntersectionOf(cs) => {
            let ps: Vec<Pl> = cs.iter().map(class).collect();
            pl(
                ps.iter().map(|p| p.pos).max().unwrap_or(0),
                sum(ps.iter().map(|p| p.neg)),
            )
        }
        C::ObjectUnionOf(cs) => {
            let ps: Vec<Pl> = cs.iter().map(class).collect();
            pl(
                sum(ps.iter().map(|p| p.pos)),
                ps.iter().map(|p| p.neg).max().unwrap_or(0),
            )
        }
        C::ObjectComplementOf(c) => {
            let p = class(c);
            pl(p.neg, p.pos)
        }
        C::ObjectOneOf(is) => pl(is.len().min(2) as u8, 0),
        C::ObjectSomeValuesFrom(_, f) => {
            let p = class(f);
            pl(p.pos.max(1), p.neg)
        }
        C::ObjectAllValuesFrom(_, f) => {
            let p = class(f);
            pl(p.pos, p.neg.max(1))
        }
        C::ObjectHasValue(..) | C::ObjectHasSelf(_) | C::DataHasValue(..) => pl(1, 0),
        C::ObjectMinCardinality(n, _, f) => {
            let p = f.as_deref().map_or(pl(0, 0), class);
            at_least(*n, p)
        }
        C::ObjectMaxCardinality(n, _, f) => {
            let p = f.as_deref().map_or(pl(0, 0), class);
            at_most(*n, p)
        }
        C::ObjectExactCardinality(n, _, f) => {
            let p = f.as_deref().map_or(pl(0, 0), class);
            both(at_least(*n, p), at_most(*n, p))
        }
        C::DataSomeValuesFrom(_, r) => {
            let p = data(r);
            pl(p.pos.max(1), p.neg)
        }
        C::DataAllValuesFrom(_, r) => {
            let p = data(r);
            pl(p.pos, p.neg.max(1))
        }
        C::DataMinCardinality(n, _, r) => at_least(*n, r.as_ref().map_or(pl(1, 0), data)),
        C::DataMaxCardinality(n, _, r) => at_most(*n, r.as_ref().map_or(pl(1, 0), data)),
        C::DataExactCardinality(n, _, r) => {
            let p = r.as_ref().map_or(pl(1, 0), data);
            both(at_least(*n, p), at_most(*n, p))
        }
    }
}

/// `≥n R.F`, F's counts being `f`: an existential in a positive position, and for n above
/// one in a negative position the disjunct `≤n-1 R.F`.
fn at_least(n: u32, f: Pl) -> Pl {
    if n == 0 {
        return pl(0, 0);
    }
    pl(f.pos.max(1), if n == 1 { f.neg } else { f.neg.max(1) })
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
fn data(r: &DataRange) -> Pl {
    match r {
        DataRange::Datatype(_) | DataRange::DatatypeRestriction(..) => pl(1, 0),
        DataRange::IntersectionOf(rs) => {
            let ps: Vec<Pl> = rs.iter().map(data).collect();
            pl(
                ps.iter().map(|p| p.pos).max().unwrap_or(0),
                sum(ps.iter().map(|p| p.neg)),
            )
        }
        DataRange::UnionOf(rs) => {
            let ps: Vec<Pl> = rs.iter().map(data).collect();
            pl(
                sum(ps.iter().map(|p| p.pos)),
                ps.iter().map(|p| p.neg).max().unwrap_or(0),
            )
        }
        DataRange::ComplementOf(r) => {
            let p = data(r);
            pl(p.neg, p.pos)
        }
        DataRange::OneOf(ls) => pl(ls.len().min(2) as u8, 0),
    }
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
        C::ObjectSomeValuesFrom(p, _) | C::ObjectHasValue(p, _) | C::ObjectHasSelf(p) => {
            !is_top_role(p)
        }
        C::ObjectMinCardinality(n, p, _) | C::ObjectExactCardinality(n, p, _) => {
            *n > 0 && !is_top_role(p)
        }
        C::DataSomeValuesFrom(ps, _) => ps.iter().all(|d| !is_top_data(d)),
        C::DataHasValue(d, _) => !is_top_data(d),
        C::DataMinCardinality(n, d, _) | C::DataExactCardinality(n, d, _) => {
            *n > 0 && !is_top_data(d)
        }
        C::ObjectAllValuesFrom(..) | C::ObjectMaxCardinality(..) => false,
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
        C::ObjectAllValuesFrom(p, _) | C::ObjectMaxCardinality(_, p, _) => !is_top_role(p),
        C::ObjectExactCardinality(0, p, _) => !is_top_role(p),
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

/// The counts of the clauses an axiom stands for, as the largest number of disjuncts of
/// one of them, and whether one of them is not ⊥-local for the empty signature. Axioms
/// about properties alone are Horn and local.
fn clauses(ax: &AxiomKind) -> (u8, bool) {
    use AxiomKind as K;
    // C ⊑ D has ¬C ⊔ D as its clause
    let sub = |c: Pl, d: Pl| add(c.neg, d.pos);
    match ax {
        K::SubClassOf(c, d) => (sub(class(c), class(d)), !bottom(c) && !top(d)),
        K::EquivalentClasses(cs) => {
            let ps: Vec<Pl> = cs.iter().map(class).collect();
            let mut most = 0;
            for (i, a) in ps.iter().enumerate() {
                for (j, b) in ps.iter().enumerate() {
                    if i != j {
                        most = most.max(sub(*a, *b));
                    }
                }
            }
            let global = !(cs.iter().all(bottom) || cs.iter().all(top));
            (most, global)
        }
        K::DisjointClasses(cs) => {
            let ps: Vec<Pl> = cs.iter().map(class).collect();
            let mut most = 0;
            for i in 0..ps.len() {
                for j in i + 1..ps.len() {
                    most = most.max(add(ps[i].neg, ps[j].neg));
                }
            }
            (most, cs.iter().filter(|c| !bottom(c)).count() > 1)
        }
        K::DisjointUnion(c, cs) => {
            let parts: Vec<Pl> = cs.iter().map(class).collect();
            let union = pl(
                sum(parts.iter().map(|p| p.pos)),
                parts.iter().map(|p| p.neg).max().unwrap_or(0),
            );
            let named = class(&ClassExpression::Class(c.clone()));
            let mut most = sub(named, union).max(sub(union, named));
            for i in 0..parts.len() {
                for j in i + 1..parts.len() {
                    most = most.max(add(parts[i].neg, parts[j].neg));
                }
            }
            // c ≡ ⊔cs is local when both sides are emptied, which a class other than
            // owl:Thing is
            let global = c.as_str() == vocab::OWL_THING || !cs.iter().all(bottom);
            (most, global)
        }
        K::ClassAssertion(c, _) => (class(c).pos, !top(c)),
        K::ObjectPropertyDomain(p, c) => (class(c).pos, is_top_role(p) && !top(c)),
        K::ObjectPropertyRange(_, c) => (class(c).pos, false),
        K::DataPropertyDomain(d, c) => (class(c).pos, is_top_data(d) && !top(c)),
        K::DataPropertyRange(_, r) => (data(r).pos, false),
        K::DatatypeDefinition(_, r) => {
            let p = data(r);
            (p.pos.max(add(p.neg, 1)), false)
        }
        K::HasKey(c, ..) => (class(c).neg, false),
        _ => (0, false),
    }
}

/// What an axiom needs: whether it is Horn and, when it is not, whether it is global.
pub fn shape(ax: &AxiomKind) -> Shape {
    let (most, global) = clauses(ax);
    let horn = most <= 1;
    Shape {
        horn,
        global: !horn && global,
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

    pub fn merge(&mut self, o: OutsideEl) {
        self.horn += o.horn;
        self.disjunctive += o.disjunctive;
        self.global += o.global;
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
                global: 1
            }
        );
    }
}
