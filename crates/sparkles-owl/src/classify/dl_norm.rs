//! The normalization of roadmap item 603's Tier D for ALCHI: the axioms outside OWL 2 EL
//! of an ontology that needs case analysis, turned into the DL-clauses of the
//! consequence-based calculus of Bate, Motik, Cuenca Grau, Tena Cucala, Simančík and
//! Horrocks (*Consequence-Based Reasoning for Description Logics with Disjunctions and
//! Number Restrictions*, JAIR 2018, Table 1).
//!
//! A class expression in a positive position becomes an expression `x` with `x ⊑ E`, and one
//! in a negative position an expression `y` with `E ⊑ y`, by the polarity rules of the
//! structural transformation, with fresh classes for what the index cannot read in place.
//! An axiom whose clause has one disjunct at most goes to the EL index as a told subsumer,
//! as the Horn normalization does, and one with two or more goes to Tier D's index of
//! clauses. A union in a positive position, a complement in a negative position and a
//! universal in a negative position are what make clauses, the last two with an empty body,
//! so that they hold in every context.
//!
//! Without number restrictions a minimum in a positive position has the models of the
//! existential restriction, up to the duplication of successors, which keeps the concepts
//! of ALCHI, so the normalization takes it as `∃R.C`, and a maximum in a negative position
//! as its dual. That is sound in every case and complete when the run takes no number
//! restriction, which this phase never does. What needs counting, nominals, data outside
//! OWL 2 EL, self restrictions and the role axioms beyond the hierarchy, inverses, symmetry,
//! transitivity and chains stay left out and reported.
//!
//! The role hierarchy is closed under inverses. A universal restriction on a role that a
//! chain or transitivity reaches, a negative existential restriction over one and a range of
//! one are unfolded by the role's automaton (Horrocks, Kutz and Sattler, *The Even More
//! Irresistible SROIQ*, KR 2006), a fresh class for each state, with the Horn tier's role
//! box. A run whose role inclusions are not regular takes nothing.

use super::horn::RoleBox;
use super::index::{Axioms, BOTTOM, Ex, Halt, Interner, NO_ROLE, Node, Role, RoleName, Scope, TOP};
use crate::model::*;
use crate::vocab;
use rustc_hash::{FxHashMap, FxHashSet};

/// What the normalization of a Tier D run adds beside the normalized axioms.
#[derive(Default)]
pub struct DlAx {
    /// the clauses with two or more disjuncts: the body's expressions, which hold of x
    /// together, and the head's, of which one does; an empty body is `owl:Thing`
    pub clauses: Vec<(Vec<Ex>, Vec<Ex>)>,
    /// for each existential restriction, the fresh class whose context is its
    /// hypothetical successor, with the empty core
    pub hyp: FxHashMap<Ex, Ex>,
    /// the at-most restrictions `M ⊑ ≤n S.B`: the holder M, n, S and B, ⊤ when unqualified
    pub atmost: Vec<(Ex, u32, Role, Ex)>,
    /// the pairs of disjoint roles
    pub rdis: Vec<(Role, Role)>,
    /// whether the run takes a number restriction, so that successors can be equal
    pub counting: bool,
    /// in a run with individuals, nominals or the universal property, the fresh class
    /// whose context is the root, where the constants' facts meet
    pub root: Option<Ex>,
    /// in a rooted run with counting, the minimum restrictions: a holder M and the
    /// existential restrictions whose successor terms differ, `M(x) → f_i(x) ≉ f_j(x)`
    pub ne: Vec<(Ex, Vec<Ex>)>,
    /// item 728's clauses for an exact or minimum cardinality in a negative position,
    /// `r_B(x, z_1) ∧ … ∧ r_B(x, z_n) → head ⊔ ⋁ z_i ≈ z_j`: the bound `n - 1` that the
    /// at-most rule counts to, the role, the filler's negative form B, ⊤ when unqualified,
    /// and the head's atoms over x, `y` with the at-least conjunction `H` of an exact one
    pub lazy: Vec<(u32, Role, Ex, Vec<Ex>)>,
    /// the at-least conjunctions `H` that a cardinality in a negative position makes,
    /// which the order of a run without the root ranks below every class
    pub low: Vec<Ex>,
    /// whether the run orders its number restrictions by entailment, which
    /// [`Limits::number_order`](super::Limits::number_order) asks of a run that counts
    /// without the root (F24, "The order between number restrictions")
    pub order: bool,
    /// with the order, the name of each minimum, a fresh class, with the conjunction of
    /// its witnesses, its told subsumer. A name that is told below an equivalent minimum's
    /// name has none, since that minimum's witnesses are its own.
    pub least: Vec<(Ex, Option<Ex>)>,
    /// with the order, for a minimum's name, the names of the minimums that entail it, a
    /// minimum at least as large over a role below its role and a filler below its
    /// filler: a context that holds one of them as a unit does not make the witnesses of
    /// this one. The relation has no cycle, since it lies within a strict order, two
    /// minimums that entail each other being ordered by their names. It need not hold
    /// every pair of that order.
    pub blockers: FxHashMap<Ex, Vec<Ex>>,
}

impl DlAx {
    pub fn bytes(&self) -> usize {
        self.clauses
            .iter()
            .map(|(b, h)| 48 + (b.capacity() + h.capacity()) * 4)
            .sum::<usize>()
            + super::saturate::table_bytes(self.hyp.capacity(), 8)
            + self.atmost.capacity() * 16
            + self.rdis.capacity() * 8
            + self.ne.capacity() * std::mem::size_of::<(Ex, Vec<Ex>)>()
            + self
                .ne
                .iter()
                .map(|(_, es)| es.capacity() * 4)
                .sum::<usize>()
            + self.least.capacity() * std::mem::size_of::<(Ex, Option<Ex>)>()
            + super::saturate::table_bytes(
                self.blockers.capacity(),
                std::mem::size_of::<(Ex, Vec<Ex>)>(),
            )
            + self
                .blockers
                .values()
                .map(|v| v.capacity() * 4)
                .sum::<usize>()
    }

    pub fn logical_bytes(&self) -> usize {
        self.clauses
            .iter()
            .map(|(b, h)| 48 + (b.len() + h.len()) * 4)
            .sum::<usize>()
            + super::saturate::table_bytes(self.hyp.len(), 8)
            + self.atmost.len() * 16
            + self.rdis.len() * 8
            + self.ne.len() * std::mem::size_of::<(Ex, Vec<Ex>)>()
            + self.ne.iter().map(|(_, es)| es.len() * 4).sum::<usize>()
            + self.least.len() * std::mem::size_of::<(Ex, Option<Ex>)>()
            + super::saturate::table_bytes(
                self.blockers.len(),
                std::mem::size_of::<(Ex, Vec<Ex>)>(),
            )
            + self.blockers.values().map(|v| v.len() * 4).sum::<usize>()
    }
}

/// Why an axiom stays left out.
type Why = &'static str;

/// Whether `dl_norm::normalize` can accept `ax` (ignoring counting/rooted flags, nesting
/// depth, and role regularity): `ax` is an axiom kind Tier D handles and every
/// subexpression that [`Norm::pos`] and [`Norm::neg`] visit avoids self restrictions and
/// `Data*` restrictions.
///
/// In particular, `ObjectMinCardinality(0, ..)` short-circuits to `TOP` in both positive
/// and negative positions without inspecting its filler, so a data restriction nested only
/// beneath a zero minimum does not prevent Tier D from taking the axiom.
pub fn may_accept(ax: &AxiomKind) -> bool {
    use AxiomKind as K;
    use ClassExpression as C;
    fn expr_ok(c: &C) -> bool {
        match c {
            C::Class(_) | C::ObjectMinCardinality(0, ..) => true,
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => cs.iter().all(expr_ok),
            C::ObjectComplementOf(f)
            | C::ObjectSomeValuesFrom(_, f)
            | C::ObjectAllValuesFrom(_, f) => expr_ok(f),
            C::ObjectMinCardinality(_, _, f)
            | C::ObjectMaxCardinality(_, _, f)
            | C::ObjectExactCardinality(_, _, f) => f.as_deref().is_none_or(expr_ok),
            C::ObjectOneOf(_) | C::ObjectHasValue(..) => true,
            C::ObjectHasSelf(_)
            | C::DataSomeValuesFrom(..)
            | C::DataAllValuesFrom(..)
            | C::DataHasValue(..)
            | C::DataMinCardinality(..)
            | C::DataMaxCardinality(..)
            | C::DataExactCardinality(..) => false,
        }
    }
    match ax {
        K::SubClassOf(a, b) => expr_ok(a) && expr_ok(b),
        K::EquivalentClasses(cs) => cs.iter().all(expr_ok),
        K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => cs.iter().all(expr_ok),
        K::ObjectPropertyDomain(_, c) | K::ObjectPropertyRange(_, c) | K::ClassAssertion(c, _) => {
            expr_ok(c)
        }
        K::ObjectPropertyAssertion(..) | K::NegativeObjectPropertyAssertion(..) => true,
        _ => false,
    }
}

/// Whether the EL part of a run lets Tier D take the axioms outside OWL 2 EL: no top or
/// bottom properties, data properties, self restrictions or reflexive roles, whose rules
/// this phase does not have, and no individuals or nominals unless `rooted` lets the run
/// take them through its root.
pub fn eligible(it: &Interner, ax: &Axioms, rooted: bool) -> bool {
    (rooted || it.individuals.is_empty())
        && ax.functional.is_empty()
        && ax.reflexive.is_empty()
        && ax.data_ranges.is_empty()
        && it.vals.is_empty()
        && !it.roles.iter().any(|r| match r {
            RoleName::Data(_) => true,
            RoleName::Object(p) => matches!(
                p.as_str(),
                vocab::OWL_TOP_OBJECT_PROPERTY | vocab::OWL_BOTTOM_OBJECT_PROPERTY
            ),
            _ => false,
        })
        && !it.nodes.iter().any(|n| match n {
            Node::Nominal(_) => !rooted,
            Node::SelfLoop(_)
            | Node::DataSome(..)
            | Node::DtAtom(_)
            | Node::DtFire(_)
            | Node::AtMost1(..) => true,
            _ => false,
        })
}

/// What one axiom adds, kept apart until the whole axiom is normalized.
#[derive(Default)]
struct Out {
    subs: Vec<(Ex, Ex)>,
    disjoint: Vec<Vec<Ex>>,
    ranges: Vec<(Role, Ex)>,
    role_subs: Vec<(Role, Role)>,
    chains: Vec<(Vec<Role>, Role)>,
    clauses: Vec<(Vec<Ex>, Vec<Ex>)>,
    atmost: Vec<(Ex, u32, Role, Ex)>,
    rdis: Vec<(Role, Role)>,
    /// the irreflexive roles, which must be simple
    irreflexive: Vec<Role>,
    /// the minimum restrictions of a rooted run with counting, as [`DlAx::ne`]
    ne: Vec<(Ex, Vec<Ex>)>,
    /// item 728's clauses of a cardinality in a negative position, as [`DlAx::lazy`]
    lazy: Vec<(u32, Role, Ex, Vec<Ex>)>,
    /// the at-least conjunctions ranked lowest, as [`DlAx::low`]
    low: Vec<Ex>,
    /// the minimums of a run that orders its number restrictions: the name, the
    /// conjunction of the witnesses, the minimum, the role and the filler
    least: Vec<(Ex, Ex, u32, Role, Ex)>,
    /// whether an inverse property occurs, which a symmetric or inverse functional
    /// property also makes
    inverse: bool,
}

#[derive(Clone, Copy)]
struct OutMark {
    subs: usize,
    disjoint: usize,
    ranges: usize,
    role_subs: usize,
    chains: usize,
    clauses: usize,
    atmost: usize,
    rdis: usize,
    irreflexive: usize,
    ne: usize,
    lazy: usize,
    low: usize,
    least: usize,
    inverse: bool,
}

impl Out {
    fn mark(&self) -> OutMark {
        OutMark {
            subs: self.subs.len(),
            disjoint: self.disjoint.len(),
            ranges: self.ranges.len(),
            role_subs: self.role_subs.len(),
            chains: self.chains.len(),
            clauses: self.clauses.len(),
            atmost: self.atmost.len(),
            rdis: self.rdis.len(),
            irreflexive: self.irreflexive.len(),
            ne: self.ne.len(),
            lazy: self.lazy.len(),
            low: self.low.len(),
            least: self.least.len(),
            inverse: self.inverse,
        }
    }

    fn truncate(&mut self, m: OutMark) {
        self.subs.truncate(m.subs);
        self.disjoint.truncate(m.disjoint);
        self.ranges.truncate(m.ranges);
        self.role_subs.truncate(m.role_subs);
        self.chains.truncate(m.chains);
        self.clauses.truncate(m.clauses);
        self.atmost.truncate(m.atmost);
        self.rdis.truncate(m.rdis);
        self.irreflexive.truncate(m.irreflexive);
        self.ne.truncate(m.ne);
        self.lazy.truncate(m.lazy);
        self.low.truncate(m.low);
        self.least.truncate(m.least);
        self.inverse = m.inverse;
    }

    fn bytes(&self) -> usize {
        self.subs.capacity() * 8
            + self
                .disjoint
                .iter()
                .map(|d| 24 + d.capacity() * 4)
                .sum::<usize>()
            + self.disjoint.capacity() * 24
            + self.ranges.capacity() * 8
            + self.role_subs.capacity() * 8
            + self
                .chains
                .iter()
                .map(|(c, _)| 32 + c.capacity() * 4)
                .sum::<usize>()
            + self.chains.capacity() * 32
            + self
                .clauses
                .iter()
                .map(|(b, h)| 48 + (b.capacity() + h.capacity()) * 4)
                .sum::<usize>()
            + self.clauses.capacity() * 48
            + self.atmost.capacity() * 16
            + self.rdis.capacity() * 8
            + self.irreflexive.capacity() * 4
            + self
                .ne
                .iter()
                .map(|(_, es)| 32 + es.capacity() * 4)
                .sum::<usize>()
            + self.ne.capacity() * 32
            + self
                .lazy
                .iter()
                .map(|(_, _, _, h)| 40 + h.capacity() * 4)
                .sum::<usize>()
            + self.lazy.capacity() * 40
            + self.low.capacity() * 4
            + self.least.capacity() * 20
    }

    fn extend(&mut self, o: Out) {
        self.subs.extend(o.subs);
        self.disjoint.extend(o.disjoint);
        self.ranges.extend(o.ranges);
        self.role_subs.extend(o.role_subs);
        self.chains.extend(o.chains);
        self.clauses.extend(o.clauses);
        self.atmost.extend(o.atmost);
        if self.rdis.is_empty() && self.rdis.capacity() == 0 {
            self.rdis = o.rdis;
        } else {
            self.rdis.extend(o.rdis);
        }
        self.irreflexive.extend(o.irreflexive);
        self.ne.extend(o.ne);
        self.lazy.extend(o.lazy);
        self.low.extend(o.low);
        self.least.extend(o.least);
        self.inverse |= o.inverse;
    }
}

struct Norm<'a, 'b> {
    it: &'a mut Interner,
    out: Out,
    /// whether the run takes number restrictions; without them a minimum is read as the
    /// existential restriction, which keeps the models of ALCHI
    counting: bool,
    /// whether the run has individuals, so that it is rooted: nominals become the
    /// singleton classes `N_u`, and what their rules leave out is refused
    rooted: bool,
    /// whether a minimum gets a name of its own, for [`number_order`]: a run that counts
    /// without the root and was asked for the order
    order: bool,
    budget: Option<&'a mut super::horn::PairBudget<'b>>,
    stop: &'a dyn Fn() -> bool,
    halt: Option<Halt>,
}

/// The largest minimum that becomes that many existential restrictions.
const MAX_MIN: u32 = 64;

/// Whether a class expression in a positive (`pos`) or negative position needs counting: a
/// maximum above zero in a positive position or a minimum above one in a negative one, and
/// in a `rooted` run also a minimum above one in a positive position and a maximum above
/// zero in a negative one, whose dual is such a minimum, since a nominal can make the
/// successors meet and the existential restriction would lose that.
fn needs_counting(c: &ClassExpression, pos: bool, rooted: bool) -> bool {
    use ClassExpression as C;
    let sub = |f: &Option<Box<ClassExpression>>, p: bool| {
        f.as_deref().is_some_and(|f| needs_counting(f, p, rooted))
    };
    match c {
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            cs.iter().any(|c| needs_counting(c, pos, rooted))
        }
        C::ObjectComplementOf(f) => needs_counting(f, !pos, rooted),
        C::ObjectSomeValuesFrom(_, f) | C::ObjectAllValuesFrom(_, f) => {
            needs_counting(f, pos, rooted)
        }
        C::ObjectMinCardinality(n, _, f) => ((!pos || rooted) && *n >= 2) || sub(f, pos),
        C::ObjectMaxCardinality(n, _, f) => ((pos || rooted) && *n >= 1) || sub(f, !pos),
        C::ObjectExactCardinality(n, _, f) => *n >= 1 || sub(f, pos) || sub(f, !pos),
        _ => false,
    }
}

/// Whether an axiom needs counting, in a `rooted` run or not.
fn axiom_counts(ax: &AxiomKind, rooted: bool) -> bool {
    use AxiomKind as K;
    let either =
        |c: &ClassExpression| needs_counting(c, false, rooted) || needs_counting(c, true, rooted);
    match ax {
        K::SubClassOf(c, d) => needs_counting(c, false, rooted) || needs_counting(d, true, rooted),
        K::EquivalentClasses(cs) => cs.iter().any(either),
        K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => cs.iter().any(either),
        K::ObjectPropertyDomain(_, c) | K::ObjectPropertyRange(_, c) => {
            needs_counting(c, true, rooted)
        }
        K::ClassAssertion(c, _) => rooted && needs_counting(c, true, rooted),
        // disjoint and asymmetric roles read the role literals of successor terms (DL9)
        K::FunctionalObjectProperty(_)
        | K::InverseFunctionalObjectProperty(_)
        | K::DisjointObjectProperties(_)
        | K::AsymmetricObjectProperty(_) => true,
        _ => false,
    }
}

fn role_in(
    it: &mut Interner,
    inverse: &mut bool,
    p: &ObjectPropertyExpression,
) -> Result<Role, Why> {
    let named = p.named();
    if matches!(
        named.as_str(),
        vocab::OWL_TOP_OBJECT_PROPERTY | vocab::OWL_BOTTOM_OBJECT_PROPERTY
    ) {
        return Err("the top and bottom object properties");
    }
    let r = it.role(RoleName::Object(named.clone()));
    let i = it.role(RoleName::Inverse(named.clone()));
    Ok(match p {
        ObjectPropertyExpression::Property(_) => r,
        ObjectPropertyExpression::InverseOf(_) => {
            *inverse = true;
            i
        }
    })
}

impl Norm<'_, '_> {
    /// The role of an object property expression, with its inverse interned beside it.
    fn role(&mut self, p: &ObjectPropertyExpression) -> Result<Role, Why> {
        role_in(self.it, &mut self.out.inverse, p)
    }

    fn conj(&mut self, mut es: Vec<Ex>) -> Ex {
        es.sort_unstable();
        es.dedup();
        es.retain(|&e| e != TOP);
        if es.contains(&BOTTOM) {
            return BOTTOM;
        }
        match es.len() {
            0 => TOP,
            1 => es[0],
            _ => {
                let mut acc = es[0];
                for &e in &es[1..] {
                    acc = self.it.conj(acc, e);
                }
                acc
            }
        }
    }

    /// The qualifier of a maximum or of item 728's clauses, ⊤ when unqualified: an atom
    /// `y` with `f ⊑ y`, which a neighbour derives when it is in `f` and which the
    /// at-most rule then counts. The negative form of an intersection or of an existential
    /// restriction is an expression that the rules build only where the left side of an
    /// inclusion or the body of a clause names it. A qualifier is neither, so a compound
    /// one gets a name of its own, with the inclusions that derive the name (roadmap
    /// item 897). Without the name no neighbour derived such a qualifier unless another
    /// axiom named the same expression, the maximum counted no neighbour, and the run
    /// reported an answer that missed the maximum's consequences as complete.
    fn qualifier(&mut self, f: Option<&ClassExpression>, d: usize) -> Result<Ex, Why> {
        let Some(f) = f else {
            return Ok(TOP);
        };
        let b = self.neg(f, d)?;
        Ok(match self.it.nodes[b as usize] {
            Node::Conj(..) => {
                let e = self.named(b);
                let n = self.it.fresh_class();
                self.out.subs.push((e, n));
                n
            }
            Node::Exist(..) => self.named(b),
            _ => b,
        })
    }

    /// The negative form `e` of a qualifier with a name in place of each existential
    /// restriction: `N` with `F ⊑ ∀r⁻.N` for `∃r.F`, which is `∃r.F ⊑ N` as the calculus's
    /// normal form writes an existential restriction in a negative position. The
    /// expression `∃r.F` itself is also the positive restriction, whose successor term a
    /// context makes wherever it derives the expression, so a neighbour that derived it
    /// from a neighbour of its own would make a copy of that neighbour, which the at-most
    /// rule would count. A rooted run has no rule for a universal restriction over an
    /// inverse role beside a nominal and a maximum, so there the name is told above the
    /// expression, `∃r.F ⊑ N`, as an axiom of the ontology would state it.
    fn named(&mut self, e: Ex) -> Ex {
        match self.it.nodes[e as usize] {
            Node::Conj(..) => {
                let mut parts = Vec::new();
                let mut todo = vec![e];
                while let Some(x) = todo.pop() {
                    match self.it.nodes[x as usize] {
                        Node::Conj(a, b) => todo.extend([a, b]),
                        _ => parts.push(x),
                    }
                }
                let parts = parts.into_iter().map(|x| self.named(x)).collect();
                self.conj(parts)
            }
            Node::Exist(r, f) => {
                let g = self.named(f);
                let n = self.it.fresh_class();
                let inverse = match self.it.roles[r as usize].clone() {
                    RoleName::Object(p) if !self.rooted => Some(self.it.role(RoleName::Inverse(p))),
                    RoleName::Inverse(p) if !self.rooted => Some(self.it.role(RoleName::Object(p))),
                    _ => None,
                };
                match inverse {
                    Some(i) => {
                        let all = self.it.node(Node::Forall(i, n));
                        self.out.subs.push((g, all));
                    }
                    None => {
                        let some = self.it.node(Node::Exist(r, g));
                        self.out.subs.push((some, n));
                    }
                }
                n
            }
            _ => e,
        }
    }

    /// `x` with `x ⊑ c`.
    fn pos(&mut self, c: &ClassExpression, depth: usize) -> Result<Ex, Why> {
        use ClassExpression as C;
        if depth > 256 {
            return Err("class expressions nested too deeply");
        }
        let d = depth + 1;
        Ok(match c {
            C::Class(c) => self.it.class(c),
            C::ObjectIntersectionOf(cs) => {
                let es = cs
                    .iter()
                    .map(|c| self.pos(c, d))
                    .collect::<Result<_, _>>()?;
                self.conj(es)
            }
            C::ObjectUnionOf(cs) => {
                let mut es: Vec<Ex> = cs
                    .iter()
                    .map(|c| self.pos(c, d))
                    .collect::<Result<_, _>>()?;
                if es.contains(&TOP) {
                    return Ok(TOP);
                }
                es.retain(|&e| e != BOTTOM);
                es.sort_unstable();
                es.dedup();
                match es.len() {
                    0 => BOTTOM,
                    1 => es[0],
                    _ => {
                        let x = self.it.fresh_class();
                        self.out.clauses.push((vec![x], es));
                        x
                    }
                }
            }
            C::ObjectComplementOf(f) => self.not(f, d)?,
            C::ObjectSomeValuesFrom(p, f) => self.some(p, f, d)?,
            C::ObjectAllValuesFrom(p, f) => {
                let r = self.role(p)?;
                let f = self.pos(f, d)?;
                if f == TOP {
                    TOP
                } else {
                    self.it.node(Node::Forall(r, f))
                }
            }
            C::ObjectMinCardinality(0, ..) => TOP,
            C::ObjectMinCardinality(n, p, f) if self.counting && *n >= 2 => {
                self.at_least(*n, p, f.as_deref(), d)?
            }
            // `A ⊑ ≥2 r.{o}` makes A empty, which the existential restriction would lose
            C::ObjectMinCardinality(n, ..) if self.rooted && *n >= 2 => {
                return Err("a minimum cardinality above one with individuals and no counting");
            }
            C::ObjectMinCardinality(_, p, f) => match f {
                Some(f) => self.some(p, f, d)?,
                None => self.some(p, &C::Class(Class::thing()), d)?,
            },
            C::ObjectMaxCardinality(0, p, f) | C::ObjectExactCardinality(0, p, f) => {
                // ≤0 r.F is ∀r.¬F
                let r = self.role(p)?;
                let nf = match f {
                    Some(f) => self.not(f, d)?,
                    None => BOTTOM,
                };
                if nf == TOP {
                    TOP
                } else {
                    self.it.node(Node::Forall(r, nf))
                }
            }
            C::ObjectMaxCardinality(n, p, f) if self.counting => {
                // M ⊑ ≤n r.B with F ⊑ B, B the negative form of F or its name
                let r = self.role(p)?;
                let b = self.qualifier(f.as_deref(), d)?;
                if b == BOTTOM {
                    TOP
                } else {
                    let m = self.it.fresh_class();
                    self.out.atmost.push((m, *n, r, b));
                    m
                }
            }
            C::ObjectExactCardinality(n, p, f) if self.counting => {
                let lo = self.pos(&C::ObjectMinCardinality(*n, p.clone(), f.clone()), d)?;
                let hi = self.pos(&C::ObjectMaxCardinality(*n, p.clone(), f.clone()), d)?;
                self.conj(vec![lo, hi])
            }
            C::ObjectMaxCardinality(..) | C::ObjectExactCardinality(..) => {
                return Err("a maximum cardinality above zero in a positive position");
            }
            C::ObjectOneOf(is) if self.rooted => {
                let mut es = self.singletons(is)?;
                match es.len() {
                    0 => BOTTOM,
                    1 => es[0],
                    _ => {
                        let x = self.it.fresh_class();
                        es.sort_unstable();
                        self.out.clauses.push((vec![x], es));
                        x
                    }
                }
            }
            C::ObjectHasValue(p, i) if self.rooted => {
                let r = self.role(p)?;
                let n = self.singletons(&std::iter::once(i.clone()).collect())?[0];
                self.it.node(Node::Exist(r, n))
            }
            C::ObjectOneOf(_) | C::ObjectHasValue(..) => return Err("individuals"),
            C::ObjectHasSelf(_) => return Err("self restrictions"),
            _ => return Err("data restrictions"),
        })
    }

    /// The singleton classes `N_u` of the individuals of an enumeration, sorted and
    /// without repeats.
    fn singletons(&mut self, is: &std::collections::BTreeSet<Individual>) -> Result<Vec<Ex>, Why> {
        let mut es = Vec::with_capacity(is.len());
        for i in is {
            es.push(self.it.individual(i));
        }
        es.sort_unstable();
        es.dedup();
        Ok(es)
    }

    /// `∃p.f` in a positive position.
    fn some(
        &mut self,
        p: &ObjectPropertyExpression,
        f: &ClassExpression,
        d: usize,
    ) -> Result<Ex, Why> {
        let r = self.role(p)?;
        let f = self.pos(f, d)?;
        Ok(if f == BOTTOM {
            BOTTOM
        } else {
            self.it.node(Node::Exist(r, f))
        })
    }

    /// `≥n p.f` in a positive position, n ≥ 2: n existential restrictions with fresh,
    /// pairwise disjoint fillers below f (Kazakov's Lemma 2, the paper's DL2 with the
    /// inequalities of the witnesses read as the disjointness of their classes).
    fn at_least(
        &mut self,
        n: u32,
        p: &ObjectPropertyExpression,
        f: Option<&ClassExpression>,
        d: usize,
    ) -> Result<Ex, Why> {
        if n > MAX_MIN {
            return Err("a minimum cardinality above 64");
        }
        let r = self.role(p)?;
        let fill = match f {
            Some(f) => self.pos(f, d)?,
            None => TOP,
        };
        if fill == BOTTOM {
            return Ok(BOTTOM);
        }
        let mut bs = Vec::with_capacity(n as usize);
        let mut es = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let b = self.it.fresh_class();
            if fill != TOP {
                self.out.subs.push((b, fill));
            }
            bs.push(b);
            es.push(self.it.node(Node::Exist(r, b)));
        }
        if self.rooted {
            // fillers disjoint everywhere are unsound once a nominal can merge them, so
            // the successor terms differ instead, under a holder of their own
            let m = self.it.fresh_class();
            let all = self.conj(es.clone());
            self.out.subs.push((m, all));
            self.out.ne.push((m, es));
            return Ok(m);
        }
        self.out.disjoint.push(bs);
        let h = self.conj(es);
        if !self.order {
            return Ok(h);
        }
        // a name of the minimum's own, which no rule builds from the witnesses, so that
        // it holds only where an axiom states the minimum
        let g = self.it.fresh_class();
        self.out.subs.push((g, h));
        self.out.least.push((g, h, n, r, fill));
        Ok(g)
    }

    /// `¬f` in a positive position: `x` with `x ⊓ y ⊑ ⊥` for f's negative form y, and
    /// `g`'s positive form for `f = ¬g`.
    fn not(&mut self, f: &ClassExpression, d: usize) -> Result<Ex, Why> {
        if let ClassExpression::ObjectComplementOf(g) = f {
            return self.pos(g, d);
        }
        let g = self.neg(f, d)?;
        Ok(match g {
            TOP => BOTTOM,
            BOTTOM => TOP,
            _ => {
                let x = self.it.fresh_class();
                let both = self.it.conj(x, g);
                self.out.subs.push((both, BOTTOM));
                x
            }
        })
    }

    /// `y` with `c ⊑ y`.
    fn neg(&mut self, c: &ClassExpression, depth: usize) -> Result<Ex, Why> {
        use ClassExpression as C;
        if depth > 256 {
            return Err("class expressions nested too deeply");
        }
        let d = depth + 1;
        Ok(match c {
            C::Class(c) => self.it.class(c),
            C::ObjectIntersectionOf(cs) => {
                // `D ⊓ ¬F ⊑ y` is the clause `D → y ⊔ F`, with a body, where the complement
                // alone would give a clause of every context; so are `∀r.F` and `≤n r.F`,
                // whose duals are `∃r.¬F` and `≥(n+1) r.F`
                let (moved, plain): (Vec<&ClassExpression>, Vec<&ClassExpression>) =
                    cs.iter().partition(|c| self.dual(c).is_some());
                if moved.is_empty() || plain.is_empty() {
                    let es = cs
                        .iter()
                        .map(|c| self.neg(c, d))
                        .collect::<Result<_, _>>()?;
                    return Ok(self.conj(es));
                }
                let mut body = plain
                    .iter()
                    .map(|c| self.neg(c, d))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut head = Vec::new();
                for c in moved {
                    let dual = self.dual(c).expect("a conjunct with a dual");
                    let h = self.pos(&dual, d)?;
                    // with counting the at-least conjunction of a moved maximum ranks
                    // lowest, item 728
                    if self.counting
                        && matches!(c, C::ObjectMaxCardinality(n, ..) if *n >= 1)
                        && h != TOP
                        && h != BOTTOM
                    {
                        self.out.low.push(h);
                    }
                    head.push(h);
                }
                if head.contains(&TOP) || body.contains(&BOTTOM) {
                    return Ok(BOTTOM);
                }
                body.retain(|&e| e != TOP);
                head.retain(|&e| e != BOTTOM);
                let y = self.it.fresh_class();
                if head.is_empty() {
                    let b = self.conj(body);
                    self.out.subs.push((b, y));
                } else {
                    head.push(y);
                    head.sort_unstable();
                    head.dedup();
                    body.sort_unstable();
                    body.dedup();
                    self.out.clauses.push((body, head));
                }
                y
            }
            C::ObjectUnionOf(cs) => {
                let mut es: Vec<Ex> = cs
                    .iter()
                    .map(|c| self.neg(c, d))
                    .collect::<Result<_, _>>()?;
                if es.contains(&TOP) {
                    return Ok(TOP);
                }
                es.retain(|&e| e != BOTTOM);
                es.sort_unstable();
                es.dedup();
                match es.len() {
                    0 => BOTTOM,
                    1 => es[0],
                    _ => {
                        let y = self.it.fresh_class();
                        for e in es {
                            self.out.subs.push((e, y));
                        }
                        y
                    }
                }
            }
            C::ObjectComplementOf(f) => {
                // ¬f ⊑ y is ⊤ ⊑ f ⊔ y
                let x = self.pos(f, d)?;
                match x {
                    TOP => BOTTOM,
                    BOTTOM => TOP,
                    _ => {
                        let y = self.it.fresh_class();
                        self.out.clauses.push((Vec::new(), vec![x, y]));
                        y
                    }
                }
            }
            C::ObjectSomeValuesFrom(p, f) => self.neg_some(p, Some(f), d)?,
            C::ObjectMinCardinality(0, ..) => TOP,
            C::ObjectMinCardinality(1, p, f) => self.neg_some(p, f.as_deref(), d)?,
            C::ObjectMinCardinality(n, p, f) if self.counting && self.rooted => {
                // ≥n p.f ⊑ y is ⊤ ⊑ y ⊔ ≤(n-1) p.f, with a holder for the maximum: a rooted
                // run keeps its rules (F24, item 728)
                let m = self.pos(&C::ObjectMaxCardinality(n - 1, p.clone(), f.clone()), d)?;
                self.global_or(m)?
            }
            C::ObjectMinCardinality(n, p, f) if self.counting => {
                // ≥n p.f ⊑ y is ⊤ ⊑ y ⊔ ≤(n-1) p.f, written as the paper's at-most clause
                // itself, `p_B(x, z_1) ∧ … ∧ p_B(x, z_n) → y ⊔ ⋁ z_i ≈ z_j`, item 728
                let r = self.role(p)?;
                let b = self.qualifier(f.as_deref(), d)?;
                if b == BOTTOM {
                    return Ok(BOTTOM);
                }
                let y = self.it.fresh_class();
                self.out.lazy.push((n - 1, r, b, vec![y]));
                y
            }
            C::ObjectMinCardinality(..) => {
                return Err("a minimum cardinality above one in a negative position");
            }
            C::ObjectAllValuesFrom(p, f) => {
                // ∀p.f ⊑ y is ⊤ ⊑ y ⊔ ∃p.¬f
                let r = self.role(p)?;
                let nf = self.not(f, d)?;
                if nf == BOTTOM {
                    return Ok(TOP);
                }
                let e = self.it.node(Node::Exist(r, nf));
                let y = self.it.fresh_class();
                self.out.clauses.push((Vec::new(), vec![e, y]));
                y
            }
            C::ObjectMaxCardinality(n, p, f) => {
                // ≤n p.f ⊑ y is ⊤ ⊑ y ⊔ ≥(n+1) p.f, which without counting is ∃p.f
                // without counting any minimum above zero reads as an existential restriction
                let n1 = match n.checked_add(1) {
                    Some(k) => k,
                    None if self.counting => return Err("a cardinality too large"),
                    None => *n,
                };
                let m = self.pos(&C::ObjectMinCardinality(n1, p.clone(), f.clone()), d)?;
                // with counting the at-least conjunction of ≥(n+1) ranks lowest, item 728
                if self.counting && *n >= 1 && m != TOP && m != BOTTOM {
                    self.out.low.push(m);
                }
                self.global_or(m)?
            }
            C::ObjectExactCardinality(0, p, f) => {
                let m = self.pos(&C::ObjectMinCardinality(1, p.clone(), f.clone()), d)?;
                self.global_or(m)?
            }
            C::ObjectExactCardinality(n, p, f) if self.counting && self.rooted => {
                // =n p.f ⊑ y is ⊤ ⊑ y ⊔ ≤(n-1) p.f ⊔ ≥(n+1) p.f, with a holder for the
                // maximum: a rooted run keeps its rules (F24, item 728)
                let lo = self.pos(&C::ObjectMaxCardinality(n - 1, p.clone(), f.clone()), d)?;
                let n1 = n.checked_add(1).ok_or("a cardinality too large")?;
                let hi = self.pos(&C::ObjectMinCardinality(n1, p.clone(), f.clone()), d)?;
                if lo == TOP || hi == TOP {
                    return Ok(BOTTOM);
                }
                let mut head: Vec<Ex> = [lo, hi].into_iter().filter(|&e| e != BOTTOM).collect();
                let y = self.it.fresh_class();
                head.push(y);
                head.sort_unstable();
                head.dedup();
                if head.len() == 1 {
                    return Ok(TOP);
                }
                self.out.clauses.push((Vec::new(), head));
                y
            }
            C::ObjectExactCardinality(n, p, f) if self.counting => {
                // =n p.f ⊑ y is ⊤ ⊑ y ⊔ ≤(n-1) p.f ⊔ ≥(n+1) p.f, with H the at-least
                // conjunction, which the order ranks lowest, item 728. For n = 1 the maximum
                // is ∀p.¬f, and the axiom is ∃p.f ⊑ y ⊔ H, an existential restriction in a
                // negative position as any other, so that a context derives y ⊔ H only once
                // it knows a neighbour in f, a successor through its context or the
                // predecessor through its own, and never from a hypothesis about the
                // predecessor. For n ≥ 2 it is the paper's at-most clause itself,
                // `p_B(x, z_1) ∧ … ∧ p_B(x, z_n) → y ⊔ H ⊔ ⋁ z_i ≈ z_j`, whose qualifier
                // has a name when it is compound.
                let r = self.role(p)?;
                let b = match f {
                    Some(f) if *n == 1 => self.neg(f, d)?,
                    _ => self.qualifier(f.as_deref(), d)?,
                };
                if b == BOTTOM {
                    return Ok(BOTTOM);
                }
                let n1 = n.checked_add(1).ok_or("a cardinality too large")?;
                let hi = self.pos(&C::ObjectMinCardinality(n1, p.clone(), f.clone()), d)?;
                if hi == TOP {
                    return Ok(BOTTOM);
                }
                let y = self.it.fresh_class();
                if hi != BOTTOM {
                    self.out.low.push(hi);
                }
                if *n == 1 {
                    let e = self.it.node(Node::Exist(r, b));
                    if hi == BOTTOM {
                        self.out.subs.push((e, y));
                    } else {
                        self.out.clauses.push((vec![e], vec![y, hi]));
                    }
                } else {
                    let mut head = vec![y];
                    if hi != BOTTOM {
                        head.push(hi);
                    }
                    self.out.lazy.push((n - 1, r, b, head));
                }
                y
            }
            C::ObjectExactCardinality(..) => {
                return Err("an exact cardinality in a negative position");
            }
            // `{a, b} ⊑ y` is `N_a ⊑ y` and `N_b ⊑ y`
            C::ObjectOneOf(is) if self.rooted => {
                let es = self.singletons(is)?;
                match es.len() {
                    0 => BOTTOM,
                    1 => es[0],
                    _ => {
                        let y = self.it.fresh_class();
                        for e in es {
                            self.out.subs.push((e, y));
                        }
                        y
                    }
                }
            }
            C::ObjectHasValue(p, i) if self.rooted => {
                let one = C::ObjectOneOf(std::iter::once(i.clone()).collect());
                self.neg(&C::ObjectSomeValuesFrom(p.clone(), Box::new(one)), d)?
            }
            C::ObjectOneOf(_) | C::ObjectHasValue(..) => return Err("individuals"),
            C::ObjectHasSelf(_) => return Err("self restrictions"),
            _ => return Err("data restrictions"),
        })
    }

    /// The dual that a conjunct in a negative position moves to the head as: `F` for `¬F`,
    /// `∃r.¬F` for `∀r.F` and `≥(n+1) r.F` for `≤n r.F`.
    fn dual(&self, c: &ClassExpression) -> Option<ClassExpression> {
        use ClassExpression as C;
        match c {
            C::ObjectMinCardinality(n, p, f) if self.counting && *n >= 2 => {
                Some(C::ObjectMaxCardinality(n - 1, p.clone(), f.clone()))
            }
            C::ObjectComplementOf(f) => Some((**f).clone()),
            C::ObjectAllValuesFrom(p, f) => Some(C::ObjectSomeValuesFrom(
                p.clone(),
                Box::new(C::ObjectComplementOf(f.clone())),
            )),
            C::ObjectMaxCardinality(n, p, f) => Some(C::ObjectMinCardinality(
                n.checked_add(1)?,
                p.clone(),
                f.clone(),
            )),
            _ => None,
        }
    }

    /// `y` for an expression in a negative position whose dual, `m`, holds in every context
    /// where it does not: `⊤ → y ⊔ m`.
    fn global_or(&mut self, m: Ex) -> Result<Ex, Why> {
        Ok(match m {
            TOP => BOTTOM,
            BOTTOM => TOP,
            _ => {
                let y = self.it.fresh_class();
                self.out.clauses.push((Vec::new(), vec![m, y]));
                y
            }
        })
    }

    fn neg_some(
        &mut self,
        p: &ObjectPropertyExpression,
        f: Option<&ClassExpression>,
        d: usize,
    ) -> Result<Ex, Why> {
        let r = self.role(p)?;
        let g = match f {
            Some(f) => self.neg(f, d)?,
            None => TOP,
        };
        Ok(if g == BOTTOM {
            BOTTOM
        } else {
            self.it.node(Node::Exist(r, g))
        })
    }

    /// `⊓lhs ⊑ ⊔rhs`.
    fn sub(&mut self, lhs: Vec<ClassExpression>, rhs: Vec<ClassExpression>) -> Result<(), Why> {
        use ClassExpression as C;
        let mut left: Vec<ClassExpression> = Vec::new();
        let mut right: Vec<ClassExpression> = Vec::new();
        let mut todo_l = lhs;
        let mut todo_r = rhs;
        let mut steps = 0usize;
        while !todo_l.is_empty() || !todo_r.is_empty() {
            steps += 1;
            if steps > 4096 {
                return Err("class expressions nested too deeply");
            }
            if let Some(c) = todo_l.pop() {
                match c {
                    C::ObjectIntersectionOf(cs) => todo_l.extend(cs),
                    // a complement, a universal or a maximum on the left is its dual on the
                    // right, which keeps the clause's body
                    c if self.dual(&c).is_some() => {
                        let dual = self.dual(&c).expect("a dual");
                        todo_r.push(dual)
                    }
                    C::Class(ref k) if k.as_str() == vocab::OWL_THING => {}
                    c => left.push(c),
                }
                continue;
            }
            if let Some(c) = todo_r.pop() {
                match c {
                    C::ObjectUnionOf(cs) => todo_r.extend(cs),
                    C::ObjectComplementOf(f) => todo_l.push(*f),
                    C::Class(ref k) if k.as_str() == vocab::OWL_NOTHING => {}
                    c => right.push(c),
                }
            }
        }
        let mut body = left
            .iter()
            .map(|c| self.neg(c, 0))
            .collect::<Result<Vec<_>, _>>()?;
        let mut head = right
            .iter()
            .map(|c| self.pos(c, 0))
            .collect::<Result<Vec<_>, _>>()?;
        if head.contains(&TOP) || body.contains(&BOTTOM) {
            return Ok(());
        }
        head.retain(|&x| x != BOTTOM);
        head.sort_unstable();
        head.dedup();
        body.retain(|&x| x != TOP);
        body.sort_unstable();
        body.dedup();
        if head.iter().any(|h| body.contains(h)) {
            return Ok(());
        }
        if head.len() <= 1 {
            let y = self.conj(body);
            let x = head.first().copied().unwrap_or(BOTTOM);
            if x != y {
                self.out.subs.push((y, x));
            }
        } else {
            self.out.clauses.push((body, head));
        }
        Ok(())
    }

    /// Whether an axiom is a role axiom the run takes before the class axioms.
    fn is_role_axiom(ax: &AxiomKind) -> bool {
        use AxiomKind as K;
        matches!(
            ax,
            K::SubObjectPropertyOf(..)
                | K::EquivalentObjectProperties(_)
                | K::InverseObjectProperties(..)
                | K::SymmetricObjectProperty(_)
                | K::TransitiveObjectProperty(_)
                | K::FunctionalObjectProperty(_)
                | K::InverseFunctionalObjectProperty(_)
                | K::DisjointObjectProperties(_)
                | K::AsymmetricObjectProperty(_)
                | K::IrreflexiveObjectProperty(_)
        )
    }

    fn role_axiom(&mut self, ax: &AxiomKind) -> Result<(), Why> {
        use AxiomKind as K;
        match ax {
            K::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(p), s) => {
                let (r, s) = (self.role(p)?, self.role(s)?);
                self.out.role_subs.push((r, s));
            }
            K::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), s) => {
                let s = self.role(s)?;
                let rs = ps
                    .iter()
                    .map(|p| self.role(p))
                    .collect::<Result<Vec<_>, _>>()?;
                match rs.len() {
                    0 => {}
                    1 => self.out.role_subs.push((rs[0], s)),
                    _ => self.out.chains.push((rs, s)),
                }
            }
            K::EquivalentObjectProperties(ps) => {
                let rs = ps
                    .iter()
                    .map(|p| self.role(p))
                    .collect::<Result<Vec<_>, _>>()?;
                for w in 0..rs.len() {
                    self.out.role_subs.push((rs[w], rs[(w + 1) % rs.len()]));
                }
            }
            K::InverseObjectProperties(p, q) => {
                let (p, q) = (self.role(p)?, self.role(&q.inverse())?);
                self.out.role_subs.push((p, q));
                self.out.role_subs.push((q, p));
            }
            K::SymmetricObjectProperty(p) => {
                let (r, i) = (self.role(p)?, self.role(&p.inverse())?);
                self.out.role_subs.push((r, i));
            }
            K::TransitiveObjectProperty(p) => {
                let r = self.role(p)?;
                self.out.chains.push((vec![r, r], r));
            }
            // these four read the counting rules, so a run that does not count leaves them out
            K::FunctionalObjectProperty(_)
            | K::InverseFunctionalObjectProperty(_)
            | K::DisjointObjectProperties(_)
            | K::AsymmetricObjectProperty(_)
                if !self.counting =>
            {
                return Err("a role axiom that needs counting");
            }
            K::FunctionalObjectProperty(p) | K::InverseFunctionalObjectProperty(p) => {
                let r = match ax {
                    K::FunctionalObjectProperty(_) => self.role(p)?,
                    _ => self.role(&p.inverse())?,
                };
                let m = self.it.fresh_class();
                self.out.atmost.push((m, 1, r, TOP));
                self.out.subs.push((TOP, m));
            }
            K::DisjointObjectProperties(ps) => {
                let other_bytes = self
                    .out
                    .bytes()
                    .saturating_sub(self.out.rdis.capacity() * 8);
                let inverse = &mut self.out.inverse;
                super::horn::emit_role_pairs(
                    ps.as_slice(),
                    false,
                    self.it,
                    |it, p| role_in(it, inverse, p),
                    &mut self.out.rdis,
                    other_bytes,
                    self.budget.as_deref_mut(),
                    self.stop,
                    &mut self.halt,
                )?;
            }
            K::AsymmetricObjectProperty(p) => {
                let (r, i) = (self.role(p)?, self.role(&p.inverse())?);
                self.out.rdis.push((r, i));
            }
            // without self restrictions, reflexive properties or nominals, which Tier D
            // leaves out, a model's tree unravelling has no loop on a simple property; a
            // transitive and symmetric one makes a loop of every edge, so the role box
            // checks that the role is simple
            K::IrreflexiveObjectProperty(_) if self.rooted => {
                return Err("irreflexive properties with individuals");
            }
            K::IrreflexiveObjectProperty(p) => {
                let r = self.role(p)?;
                self.out.irreflexive.push(r);
            }
            _ => return Err("not a role axiom"),
        }
        Ok(())
    }

    fn axiom(&mut self, ax: &AxiomKind) -> Result<(), Why> {
        use AxiomKind as K;
        use ClassExpression as C;
        match ax {
            K::SubClassOf(c, d) => self.sub(vec![c.clone()], vec![d.clone()]),
            K::EquivalentClasses(cs) => {
                let cs: Vec<&ClassExpression> = cs.iter().collect();
                for w in 0..cs.len() {
                    let next = cs[(w + 1) % cs.len()];
                    if cs[w] != next {
                        self.sub(vec![cs[w].clone()], vec![next.clone()])?;
                    }
                }
                Ok(())
            }
            K::DisjointClasses(cs) => {
                let (unique, repeated) = super::dl::split_multiset(cs);
                for &r in &repeated {
                    self.empty(r)?;
                }
                if unique.len() >= 2 {
                    let mut es = unique
                        .iter()
                        .map(|c| self.neg(c, 0))
                        .collect::<Result<Vec<_>, _>>()?;
                    es.sort_unstable();
                    let mut last_empty = None;
                    for w in es.windows(2) {
                        if w[0] == w[1] && w[0] != BOTTOM && last_empty != Some(w[0]) {
                            self.out.subs.push((w[0], BOTTOM));
                            last_empty = Some(w[0]);
                        }
                    }
                    es.dedup();
                    es.retain(|&e| e != BOTTOM);
                    if es.len() >= 2 {
                        self.out.disjoint.push(es);
                    }
                }
                Ok(())
            }
            K::DisjointUnion(c, ds) => {
                let (unique, repeated) = super::dl::split_multiset(ds);
                let c = C::Class(c.clone());
                self.sub(
                    vec![c.clone()],
                    unique.iter().map(|d| (*d).clone()).collect(),
                )?;
                for &d in &unique {
                    self.sub(vec![d.clone()], vec![c.clone()])?;
                }
                for &r in &repeated {
                    self.empty(r)?;
                }
                if unique.len() >= 2 {
                    let surviving: Multiset<ClassExpression> =
                        unique.into_iter().cloned().collect();
                    self.axiom(&K::DisjointClasses(surviving))?;
                }
                Ok(())
            }
            K::ObjectPropertyDomain(p, c) => self.sub(
                vec![C::ObjectSomeValuesFrom(
                    p.clone(),
                    Box::new(C::Class(Class::thing())),
                )],
                vec![c.clone()],
            ),
            K::ObjectPropertyRange(p, c) => {
                let r = self.role(p)?;
                let c = self.pos(c, 0)?;
                if c != TOP {
                    self.out.ranges.push((r, c));
                }
                Ok(())
            }
            // the assertions outside OWL 2 EL, read as singleton inclusions
            K::ClassAssertion(c, i) if self.rooted => {
                self.sub(vec![C::ObjectOneOf([i.clone()].into())], vec![c.clone()])
            }
            K::ObjectPropertyAssertion(p, a, b) if self.rooted => self.sub(
                vec![C::ObjectOneOf([a.clone()].into())],
                vec![C::ObjectHasValue(p.clone(), b.clone())],
            ),
            K::NegativeObjectPropertyAssertion(p, a, b) if self.rooted => self.sub(
                vec![
                    C::ObjectOneOf([a.clone()].into()),
                    C::ObjectHasValue(p.clone(), b.clone()),
                ],
                vec![C::Class(Class::nothing())],
            ),
            _ => Err("an axiom Tier D does not take"),
        }
    }

    /// `c ⊑ ⊥`, using `neg` directly when it succeeds so wide shallow conjunctions do not
    /// hit `sub`'s flattening step cap, and falling back to `sub` otherwise.
    fn empty(&mut self, c: &ClassExpression) -> Result<(), Why> {
        let mark = self.out.mark();
        match self.neg(c, 0) {
            Ok(e) => {
                if e != BOTTOM {
                    self.out.subs.push((e, BOTTOM));
                }
                Ok(())
            }
            Err(_) => {
                self.out.truncate(mark);
                self.sub(vec![c.clone()], vec![])
            }
        }
    }
}

/// What the caller lets the normalization of a Tier D run take.
#[derive(Clone, Copy)]
pub struct Allowed {
    /// number restrictions and the other axioms that need counting
    pub counting: bool,
    /// individuals and nominals, through the root of a rooted run
    pub rooted: bool,
    /// the order between the number restrictions of a run that counts without the root
    pub order: bool,
    /// how far the automata that unfold the universals are reduced, as
    /// [`super::horn::dl_reduction`] reads the levels
    pub automata: u8,
}

impl Default for Allowed {
    fn default() -> Self {
        Allowed {
            counting: false,
            rooted: false,
            order: false,
            automata: 2,
        }
    }
}

/// The normalization of a Tier D run: the axioms outside OWL 2 EL that it takes, with the
/// clauses of case analysis, after the EL axioms are in `ax`. `None` when it takes no axiom
/// that needs case analysis, or when the role inclusions are not regular or their automata
/// pass the limit, and then `ax` is as it was, the interner aside. The answer's flags say
/// which axioms were taken. A run that does not allow counting leaves the axioms that need
/// it out, as the normalization did before it counted.
pub fn normalize(
    axioms: &[&AxiomKind],
    it: &mut Interner,
    ax: &mut Axioms,
    limit: Option<usize>,
    stop: &dyn Fn() -> bool,
    allowed: Allowed,
) -> Result<Option<(Vec<bool>, DlAx)>, Halt> {
    normalize_within(axioms, it, ax, limit, stop, allowed, ORDER_RELATIONS)
}

/// [`normalize`] with the most relations that [`number_order`] adds.
fn normalize_within(
    axioms: &[&AxiomKind],
    it: &mut Interner,
    ax: &mut Axioms,
    limit: Option<usize>,
    stop: &dyn Fn() -> bool,
    allowed: Allowed,
    relations: usize,
) -> Result<Option<(Vec<bool>, DlAx)>, Halt> {
    let mut taken = vec![false; axioms.len()];
    // individuals make the run rooted: those the EL part interned before the
    // normalization, and those the axioms outside it hold, anonymous ones among them
    let rooted = allowed.rooted
        && (!it.individuals.is_empty() || axioms.iter().any(|k| super::index::holds_individual(k)));
    let counting = allowed.counting && axioms.iter().any(|k| axiom_counts(k, rooted));
    let order = allowed.order && counting && !rooted;
    // every object property has its inverse
    let named: Vec<ObjectProperty> = it
        .roles
        .iter()
        .filter_map(|n| match n {
            RoleName::Object(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    for p in named {
        it.role(RoleName::Inverse(p));
    }
    let mut all = Out::default();
    let mut all_chain_inner_bytes = 0usize;
    let mut budget = super::horn::PairBudget::new(limit, ax);
    for (i, k) in axioms.iter().enumerate() {
        if !Norm::is_role_axiom(k) {
            continue;
        }
        // the fresh classes of an axiom are those of its last run over this interner
        // that normalized it, so that the contexts kept from that run name the same
        // classes (roadmap item 753)
        let scope = Scope::Dl(it.scope_axiom(k));
        it.begin_scope(scope);
        let mut n = Norm {
            it,
            out: Out::default(),
            counting,
            rooted,
            order,
            budget: Some(&mut budget),
            stop,
            halt: None,
        };
        let ok = n.role_axiom(k).is_ok();
        let halted = n.halt;
        n.it.end_scope();
        if let Some(h) = halted {
            return Err(h);
        }
        if ok {
            let out = n.out;
            if budget.is_bounded() {
                if !out.rdis.is_empty() && (!all.rdis.is_empty() || all.rdis.capacity() > 0) {
                    let old_cap = all.rdis.capacity();
                    let needed = all.rdis.len().saturating_add(out.rdis.len());
                    if needed > old_cap {
                        let next_cap = old_cap.saturating_mul(2).max(needed).max(4);
                        let growth_bytes = next_cap
                            .saturating_sub(old_cap)
                            .saturating_mul(std::mem::size_of::<(Role, Role)>());
                        budget.check(it, out.bytes().saturating_add(growth_bytes))?;
                    }
                }
                if !out.chains.is_empty() {
                    all_chain_inner_bytes = all_chain_inner_bytes.saturating_add(
                        out.chains
                            .iter()
                            .map(|(c, _)| 32 + c.capacity() * 4)
                            .sum::<usize>(),
                    );
                }
            }
            all.extend(out);
            if budget.is_bounded() {
                let retained = all.role_subs.capacity() * 8
                    + all.chains.capacity() * 32
                    + all_chain_inner_bytes
                    + all.atmost.capacity() * 16
                    + all.subs.capacity() * 8
                    + all.rdis.capacity() * 8
                    + all.irreflexive.capacity() * 4;
                budget.set_accum(retained);
            }
            taken[i] = true;
        }
    }
    let mut disjunctive = 0;
    for (i, k) in axioms.iter().enumerate() {
        if taken[i] || Norm::is_role_axiom(k) {
            continue;
        }
        let scope = Scope::Dl(it.scope_axiom(k));
        it.begin_scope(scope);
        let mut n = Norm {
            it,
            out: Out::default(),
            counting,
            rooted,
            order,
            budget: None,
            stop,
            halt: None,
        };
        let ok = n.axiom(k).is_ok();
        let halted = n.halt;
        n.it.end_scope();
        if let Some(h) = halted {
            return Err(h);
        }
        if ok {
            if !super::dl::shape(k).horn {
                disjunctive += 1;
            }
            all.extend(n.out);
            taken[i] = true;
        }
    }
    if disjunctive == 0 {
        return Ok(None);
    }
    // the Nom rule, which a nominal, an inverse and an at-most restriction need together,
    // is not built
    if rooted && all.inverse && (!all.atmost.is_empty() || !all.lazy.is_empty()) {
        return Ok(None);
    }
    // the role box: the inclusions closed under inverses, with the chains
    let inv = super::horn::inverses(it);
    let mut role_subs: Vec<(Role, Role)> = ax.role_subs.clone();
    role_subs.extend(all.role_subs.iter().copied());
    let n = role_subs.len();
    for k in 0..n {
        let (r, s) = role_subs[k];
        if let (Some(&a), Some(&b)) = (inv.get(r as usize), inv.get(s as usize))
            && a != NO_ROLE
            && b != NO_ROLE
        {
            role_subs.push((a, b));
        }
    }
    role_subs.sort_unstable();
    role_subs.dedup();
    let mut chains = ax.chains.clone();
    chains.extend(all.chains.iter().cloned());
    let mut rb = RoleBox::new(it, &role_subs, &chains, limit, stop)?;
    let roles = it.roles.len() as Role;
    for r in 0..roles {
        if !rb.simple(r) && rb.auto(r, &mut Vec::new()).is_none() {
            return Ok(None);
        }
    }
    // number restrictions and disjointness need simple roles, which OWL 2 DL asks
    if all.atmost.iter().any(|&(_, _, r, _)| !rb.simple(r))
        || all.lazy.iter().any(|&(_, r, _, _)| !rb.simple(r))
        || all
            .rdis
            .iter()
            .any(|&(a, b)| !rb.simple(a) || !rb.simple(b))
        || all.irreflexive.iter().any(|&r| !rb.simple(r))
    {
        return Ok(None);
    }
    // the clauses read their atoms in place, so the axioms commit now
    ax.role_subs = role_subs;
    ax.chains.extend(all.chains);
    ax.subs.extend(all.subs);
    ax.disjoint.extend(all.disjoint);
    ax.ranges.extend(all.ranges);
    // a domain of r is a range of r⁻, which the successors of r⁻ take at once
    let domains: Vec<(Role, Ex)> = ax
        .subs
        .iter()
        .filter_map(|&(a, c)| match it.nodes[a as usize] {
            Node::Exist(r, TOP) => Some((inv[r as usize], c)).filter(|&(i, _)| i != NO_ROLE),
            _ => None,
        })
        .collect();
    // a range of r holds of the predecessors of the links that run against r: ∃r⁻.⊤ ⊑ c
    let mut back: Vec<(Ex, Ex)> = Vec::new();
    for &(r, c) in &ax.ranges {
        if let Some(&i) = inv.get(r as usize)
            && i != NO_ROLE
        {
            back.push((it.node(Node::Exist(i, TOP)), c));
        }
    }
    ax.ranges.extend(domains);
    ax.subs.extend(back);
    // a range of a role that a chain reaches holds along the chain's paths too
    let nonsimple_ranges: Vec<(Role, Ex)> = ax
        .ranges
        .iter()
        .copied()
        .filter(|&(r, _)| !rb.simple(r))
        .collect();
    for (r, c) in nonsimple_ranges {
        let e = it.node(Node::Forall(r, c));
        ax.subs.push((TOP, e));
    }
    let (blockers, spared) = if order {
        number_order(it, ax, &all.least, &all.atmost, relations, stop)?
    } else {
        Default::default()
    };
    unfold(it, ax, &all.clauses, &mut rb, allowed.automata);
    let mut dl = DlAx {
        clauses: all.clauses,
        atmost: all.atmost,
        rdis: all.rdis,
        counting,
        ne: all.ne,
        lazy: all.lazy,
        low: all.low,
        order,
        blockers,
        least: all
            .least
            .iter()
            .map(|l| (l.0, (!spared.contains(&l.0)).then_some(l.1)))
            .collect(),
        ..DlAx::default()
    };
    // the hypothetical successor of each existential restriction
    let exists: Vec<Ex> = it
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| matches!(n, Node::Exist(..)))
        .map(|(e, _)| e as Ex)
        .collect();
    for e in exists {
        it.begin_scope(Scope::Hyp(e));
        let h = it.fresh_class();
        it.end_scope();
        dl.hyp.insert(e, h);
    }
    if rooted {
        dl.root = Some(it.fresh_class());
    }
    Ok(Some((taken, dl)))
}

/// The most told inclusions, disjointness sets and blockers that [`number_order`] adds.
/// Each is a consequence of the ontology that the run can do without, so past the bound
/// the rest are left out. The sorts and the cycle check that follow are linear in the
/// minimums and in this bound, up to a logarithm, and do not poll the stop.
const ORDER_RELATIONS: usize = 1 << 20;

/// The order of entailment between the number restrictions of a run that counts without
/// the root (F24, "The order between number restrictions"). `≥m r.F` entails `≥n s.G`
/// when `m ≥ n`, `r ⊑ s` and `F ⊑ G`, and it contradicts `≤k s.G` when `m > k`. The
/// inclusions are the told ones, closed under reflexivity and transitivity, with the
/// operands of an intersection and `owl:Thing` above every filler, so each relation is a
/// consequence of the ontology and entailment is a preorder.
///
/// The minimums with one bound, role and filler are equivalent, and the one with the
/// least name stands for them: each of the others is told equivalent to it and has it as
/// its only blocker. Between the representatives, the stronger minimum gets the weaker one
/// as a told subsumer, and the answer lists for each name those that entail it, with two
/// that entail each other ordered by name, so that the blockers lie within a strict order
/// and have no cycle. A minimum and a maximum that it contradicts are a disjointness set.
///
/// The told inclusions, blockers and disjointness sets together number at most `bound`.
/// Each is a consequence of the ontology that the run can do without, and a minimum
/// without a blocker makes its witnesses, so the rest are left out. A minimum that is told
/// equivalent to its representative gets both inclusions and the blocker together or
/// none of them.
///
/// A name that is told below one of its blockers, as a name is below its representative
/// and the larger of two equivalent representatives below the smaller, loses its
/// inclusion into its own witnesses: wherever it holds, under a clause or as a unit, the
/// blocker holds and has witnesses, or a blocker of its own. The second answer lists
/// these names.
#[allow(clippy::type_complexity)]
fn number_order(
    it: &Interner,
    ax: &mut Axioms,
    least: &[(Ex, Ex, u32, Role, Ex)],
    atmost: &[(Ex, u32, Role, Ex)],
    bound: usize,
    stop: &dyn Fn() -> bool,
) -> Result<(FxHashMap<Ex, Vec<Ex>>, FxHashSet<Ex>), Halt> {
    let mut out: FxHashMap<Ex, Vec<Ex>> = FxHashMap::default();
    if least.is_empty() {
        return Ok((out, FxHashSet::default()));
    }
    let mut steps = 0usize;
    let mut poll = |n: usize| -> Result<(), Halt> {
        steps += n;
        if steps >= 1 << 16 {
            steps = 0;
            if stop() {
                return Err(Halt::Stopped);
            }
        }
        Ok(())
    };
    // the told superroles of the minimums' roles and the told subsumers of their fillers,
    // sorted
    let mut role_edges: FxHashMap<Role, Vec<Role>> = FxHashMap::default();
    for &(x, y) in &ax.role_subs {
        role_edges.entry(x).or_default().push(y);
    }
    let mut told: FxHashMap<Ex, Vec<Ex>> = FxHashMap::default();
    for &(a, b) in &ax.subs {
        told.entry(a).or_default().push(b);
    }
    let mut role_up: FxHashMap<Role, Vec<Role>> = FxHashMap::default();
    let mut up: FxHashMap<Ex, Vec<Ex>> = FxHashMap::default();
    for &(_, _, _, r, f) in least {
        if let std::collections::hash_map::Entry::Vacant(slot) = role_up.entry(r) {
            let mut seen: FxHashSet<Role> = std::iter::once(r).collect();
            let mut todo = vec![r];
            while let Some(a) = todo.pop() {
                for &y in role_edges.get(&a).map_or(&[][..], |v| v.as_slice()) {
                    if seen.insert(y) {
                        todo.push(y);
                    }
                }
                poll(1)?;
            }
            let mut v: Vec<Role> = seen.into_iter().collect();
            v.sort_unstable();
            slot.insert(v);
        }
        if let std::collections::hash_map::Entry::Vacant(slot) = up.entry(f) {
            // `owl:Thing` is above every filler, and so is what the ontology tells of it
            let mut seen: FxHashSet<Ex> = [f, TOP].into_iter().collect();
            let mut todo: Vec<Ex> = seen.iter().copied().collect();
            while let Some(a) = todo.pop() {
                if let Node::Conj(x, y) = it.nodes[a as usize] {
                    for z in [x, y] {
                        if seen.insert(z) {
                            todo.push(z);
                        }
                    }
                }
                for &b in told.get(&a).map_or(&[][..], |v| v.as_slice()) {
                    if seen.insert(b) {
                        todo.push(b);
                    }
                }
                poll(1)?;
            }
            let mut v: Vec<Ex> = seen.into_iter().collect();
            v.sort_unstable();
            slot.insert(v);
        }
    }
    // the representatives by role and filler, with their bounds, and the names each
    // stands for; the maximums by role and filler
    let mut hs: Vec<(Role, Ex, u32, Ex)> = least.iter().map(|l| (l.3, l.4, l.2, l.0)).collect();
    hs.sort_unstable();
    hs.dedup();
    let mut mins: FxHashMap<(Role, Ex), Vec<(u32, Ex)>> = FxHashMap::default();
    let mut stands: FxHashMap<Ex, Vec<Ex>> = FxHashMap::default();
    let mut reps: Vec<(Role, Ex, u32, Ex)> = Vec::new();
    let mut subs: Vec<(Ex, Ex)> = Vec::new();
    let mut relations = 0usize;
    for &(r, f, n, g) in &hs {
        poll(1)?;
        match reps.last() {
            Some(&(r0, f0, n0, g0)) if (r0, f0, n0) == (r, f, n) => {
                if relations + 3 <= bound {
                    relations += 3;
                    subs.push((g, g0));
                    subs.push((g0, g));
                    out.insert(g, vec![g0]);
                    stands.entry(g0).or_default().push(g);
                }
            }
            _ => {
                reps.push((r, f, n, g));
                mins.entry((r, f)).or_default().push((n, g));
            }
        }
    }
    let mut maxs: FxHashMap<(Role, Ex), Vec<(u32, Ex)>> = FxHashMap::default();
    for &(m, k, s, b) in atmost {
        maxs.entry((s, b)).or_default().push((k, m));
    }
    let entails = |a: &(Role, Ex, u32, Ex), b: &(Role, Ex, u32, Ex)| {
        a.2 >= b.2
            && role_up[&a.0].binary_search(&b.0).is_ok()
            && up[&a.1].binary_search(&b.1).is_ok()
    };
    let mut clash: Vec<Vec<Ex>> = Vec::new();
    'reps: for a in &reps {
        for &s in &role_up[&a.0] {
            for &g in &up[&a.1] {
                poll(1)?;
                for &(n, b) in mins.get(&(s, g)).map_or(&[][..], |v| v.as_slice()) {
                    poll(1)?;
                    if b == a.3 || a.2 < n {
                        continue;
                    }
                    let blocks = !entails(&(s, g, n, b), a) || a.3 < b;
                    if relations + 1 + usize::from(blocks) > bound {
                        break 'reps;
                    }
                    relations += 1;
                    subs.push((a.3, b));
                    if blocks {
                        relations += 1;
                        out.entry(b).or_default().push(a.3);
                    }
                }
                for &(k, m) in maxs.get(&(s, g)).map_or(&[][..], |v| v.as_slice()) {
                    poll(1)?;
                    if a.2 <= k {
                        continue;
                    }
                    for &h in std::iter::once(&a.3)
                        .chain(stands.get(&a.3).map_or(&[][..], |v| v.as_slice()))
                    {
                        if relations >= bound {
                            break 'reps;
                        }
                        relations += 1;
                        let mut set = vec![m, h];
                        set.sort_unstable();
                        clash.push(set);
                    }
                }
            }
        }
    }
    for v in out.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    // entailment is a preorder, so the blockers, which follow strict entailment and
    // otherwise the names, have no cycle; a cycle would leave a minimum without
    // witnesses, so the run blocks nothing if one shows
    if !acyclic(&out) {
        debug_assert!(
            false,
            "the blockers of the number restrictions have a cycle"
        );
        out.clear();
    }
    // the names told below a blocker, and their inclusions into their own witnesses
    let told: FxHashSet<(Ex, Ex)> = subs.iter().copied().collect();
    let spared: FxHashSet<Ex> = out
        .iter()
        .filter(|(g, hs)| hs.iter().any(|&h| told.contains(&(**g, h))))
        .map(|(&g, _)| g)
        .collect();
    if !spared.is_empty() {
        let own: FxHashSet<(Ex, Ex)> = least
            .iter()
            .filter(|l| spared.contains(&l.0))
            .map(|l| (l.0, l.1))
            .collect();
        ax.subs.retain(|p| !own.contains(p));
    }
    ax.subs.extend(subs);
    clash.sort_unstable();
    clash.dedup();
    ax.disjoint.extend(clash);
    Ok((out, spared))
}

/// Whether the relation from each key to its values has no cycle.
fn acyclic(edges: &FxHashMap<Ex, Vec<Ex>>) -> bool {
    // 1 while a node is on the path, 2 once its descendants are done
    let mut state: FxHashMap<Ex, u8> = FxHashMap::default();
    for &start in edges.keys() {
        if state.contains_key(&start) {
            continue;
        }
        state.insert(start, 1);
        let mut path: Vec<(Ex, usize)> = vec![(start, 0)];
        while let Some(&mut (e, ref mut at)) = path.last_mut() {
            let next = edges.get(&e).and_then(|v| v.get(*at)).copied();
            *at += 1;
            match next {
                None => {
                    state.insert(e, 2);
                    path.pop();
                }
                Some(b) => match state.get(&b) {
                    Some(1) => return false,
                    Some(_) => {}
                    None => {
                        state.insert(b, 1);
                        path.push((b, 0));
                    }
                },
            }
        }
    }
    true
}

/// The universals on roles that are not simple, unfolded by the roles' automata: those that
/// occur positively, and those that the negative existential restrictions over such a role
/// stand for, `∃s.F ⊑ e` being `F ⊑ ∀s⁻.e`. Each state of an automaton without
/// ε-transitions is a fresh class: `B ⊑ X_start`, `X_p ⊑ ∀l.X_q` for each transition and
/// `X_final ⊑ C`. The universal itself stays, which is sound on the direct links.
///
/// At `level` 1 and above the automaton first merges the states that accept the same words
/// by the same transitions, and at 2 it also drops a transition by `l` beside one by a
/// superrole of `l` from the same state into the same state (roadmap item 712). The rules
/// apply a universal on a role to every link and backward link under it, so a state stands
/// for the same universal after either reduction (F24, "The disjunctive rules").
fn unfold(
    it: &mut Interner,
    ax: &mut Axioms,
    clauses: &[(Vec<Ex>, Vec<Ex>)],
    rb: &mut RoleBox,
    level: u8,
) {
    let inv = super::horn::inverses(it);
    // the expressions in positive and in negative positions
    let mut pos: Vec<Ex> = ax.subs.iter().map(|&(_, c)| c).collect();
    let mut neg: Vec<Ex> = ax.subs.iter().map(|&(a, _)| a).collect();
    neg.extend(ax.disjoint.iter().flatten().copied());
    for (b, h) in clauses {
        neg.extend(b.iter().copied());
        pos.extend(h.iter().copied());
    }
    let mut seen: FxHashSet<(Ex, bool)> = FxHashSet::default();
    let mut todo: Vec<(Ex, bool)> = pos.into_iter().map(|e| (e, true)).collect();
    todo.extend(neg.into_iter().map(|e| (e, false)));
    // (holder, role, filler) of each universal to unfold
    let mut unfolds: Vec<(Ex, Role, Ex)> = Vec::new();
    while let Some((e, p)) = todo.pop() {
        if !seen.insert((e, p)) {
            continue;
        }
        match it.nodes[e as usize] {
            Node::Conj(a, b) => {
                todo.push((a, p));
                todo.push((b, p));
            }
            Node::Exist(r, f) => {
                todo.push((f, p));
                if !p
                    && let Some(&i) = inv.get(r as usize)
                    && i != NO_ROLE
                    && !rb.simple(i)
                {
                    unfolds.push((f, i, e));
                }
            }
            Node::Forall(r, f) => {
                todo.push((f, p));
                if p && !rb.simple(r) {
                    unfolds.push((e, r, f));
                }
            }
            _ => {}
        }
    }
    unfolds.sort_unstable();
    unfolds.dedup();
    let mut autos: FxHashMap<Role, super::horn::EpsFree> = FxHashMap::default();
    // one bound for every automaton, since it reads the whole role hierarchy
    let cost = if level > 0 && !unfolds.is_empty() {
        rb.sub_cost()
    } else {
        0
    };
    for (holder, r, f) in unfolds {
        let Some(nfa) = rb.auto(r, &mut Vec::new()) else {
            continue;
        };
        let a = autos.entry(r).or_insert_with(|| {
            let mut a = nfa.eps_free();
            if level > 0 {
                a.reduce(level > 1, &|x, y| rb.sub(x, y), cost);
            }
            a
        });
        // a universal's holder is the universal, and a negative existential
        // restriction's is its filler, so the restriction names the scope
        let key = match it.nodes[holder as usize] {
            Node::Forall(t, c) if t == r && c == f => holder,
            _ => f,
        };
        it.begin_scope(Scope::DlUnfold(key));
        let states: Vec<Ex> = (0..a.states).map(|_| it.fresh_class()).collect();
        it.end_scope();
        ax.subs.push((holder, states[a.start as usize]));
        for (s, &x) in states.iter().enumerate() {
            if a.finals[s] {
                ax.subs.push((x, f));
            }
        }
        for &(p, l, q) in &a.trans {
            let x = it.node(Node::Forall(l, states[q as usize]));
            ax.subs.push((states[p as usize], x));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> ClassExpression {
        ClassExpression::Class(Class::new(format!("http://ex.org/{s}")))
    }

    fn run(axioms: &[AxiomKind]) -> Option<(Vec<bool>, DlAx, Axioms, Interner)> {
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        let refs: Vec<&AxiomKind> = axioms.iter().collect();
        let allowed = Allowed {
            counting: true,
            ..Allowed::default()
        };
        normalize(&refs, &mut it, &mut ax, None, &|| false, allowed)
            .unwrap()
            .map(|(t, d)| (t, d, ax, it))
    }

    fn run_rooted(axioms: &[AxiomKind]) -> Option<(Vec<bool>, DlAx, Axioms, Interner)> {
        run_rooted_counting(axioms, true)
    }

    fn run_rooted_counting(
        axioms: &[AxiomKind],
        counting: bool,
    ) -> Option<(Vec<bool>, DlAx, Axioms, Interner)> {
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        let refs: Vec<&AxiomKind> = axioms.iter().collect();
        let allowed = Allowed {
            counting,
            rooted: true,
            order: false,
            ..Allowed::default()
        };
        normalize(&refs, &mut it, &mut ax, None, &|| false, allowed)
            .unwrap()
            .map(|(t, d)| (t, d, ax, it))
    }

    fn ind(s: &str) -> Individual {
        Individual::Named(NamedIndividual::new(format!("http://ex.org/{s}")))
    }

    fn one_of(is: &[&str]) -> ClassExpression {
        ClassExpression::ObjectOneOf(is.iter().map(|i| ind(i)).collect())
    }

    fn role(s: &str) -> ObjectPropertyExpression {
        ObjectPropertyExpression::Property(ObjectProperty::new(format!("http://ex.org/{s}")))
    }

    /// `A ⊑ B ⊔ C`, so that the normalization has a disjunctive axiom to take.
    fn union() -> AxiomKind {
        AxiomKind::SubClassOf(
            c("A"),
            ClassExpression::ObjectUnionOf(std::collections::BTreeSet::from([c("B"), c("C")])),
        )
    }

    fn nominal(it: &Interner, e: Ex) -> bool {
        matches!(it.nodes[e as usize], Node::Nominal(_))
    }

    /// The normalization of a run that counts without the root, with the order between
    /// its number restrictions or without it, from functional-style axioms over the prefix
    /// `http://ex.org/`. A maximum over a role of its own makes the run count, which
    /// minimums in positive positions alone do not, and a union makes it disjunctive.
    fn run_order(body: &str, order: bool) -> (DlAx, Axioms, Interner) {
        let (taken, d, ax, it) = run_order_within(body, order, ORDER_RELATIONS);
        assert!(taken.iter().all(|&t| t));
        (d, ax, it)
    }

    /// As [`run_order`], with the most relations that the order adds and the axioms taken.
    fn run_order_within(
        body: &str,
        order: bool,
        relations: usize,
    ) -> (Vec<bool>, DlAx, Axioms, Interner) {
        let doc = crate::ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
             Ontology(\n{body}\nSubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
             SubClassOf(:Z ObjectMaxCardinality(5 :zq))\n)"
        ))
        .unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        let allowed = Allowed {
            counting: true,
            rooted: false,
            order,
            ..Allowed::default()
        };
        let (taken, d) = normalize_within(
            &kinds,
            &mut it,
            &mut ax,
            None,
            &|| false,
            allowed,
            relations,
        )
        .unwrap()
        .unwrap();
        (taken, d, ax, it)
    }

    /// The name of the minimum that `class` is told to be below.
    fn least_of(d: &DlAx, ax: &Axioms, it: &mut Interner, class: &str) -> Ex {
        let x = it.class(&Class::new(format!("http://ex.org/{class}")));
        ax.subs
            .iter()
            .find(|&&(a, b)| a == x && d.least.iter().any(|&(g, _)| g == b))
            .map(|&(_, b)| b)
            .unwrap()
    }

    #[test]
    fn without_the_order_a_minimum_has_no_name() {
        let body = "SubClassOf(:A ObjectMinCardinality(3 :r :F))
                    SubClassOf(:B ObjectMinCardinality(2 :r :F))
                    SubClassOf(:C ObjectMaxCardinality(1 :r :F))";
        let (d, _, _) = run_order(body, false);
        assert!(!d.order && d.least.is_empty() && d.blockers.is_empty());
        let (d, _, _) = run_order(body, true);
        assert!(d.order);
        assert_eq!(d.least.len(), 2);
    }

    #[test]
    fn a_larger_minimum_blocks_a_smaller_one_and_contradicts_a_maximum_below_it() {
        let (d, ax, mut it) = run_order(
            "SubClassOf(:F :G) SubObjectPropertyOf(:r :s)
             SubClassOf(:A ObjectMinCardinality(3 :r :F))
             SubClassOf(:B ObjectMinCardinality(2 :s :G))
             SubClassOf(:C ObjectMaxCardinality(2 :s :G))
             SubClassOf(:D ObjectMaxCardinality(3 :s :G))",
            true,
        );
        let (a, b) = (
            least_of(&d, &ax, &mut it, "A"),
            least_of(&d, &ax, &mut it, "B"),
        );
        assert_eq!(d.blockers.get(&b), Some(&vec![a]));
        assert!(!d.blockers.contains_key(&a));
        assert!(ax.subs.contains(&(a, b)) && !ax.subs.contains(&(b, a)));
        // the maximum of two contradicts the minimum of three, and the maximum of three
        // contradicts neither
        let holders: Vec<Ex> = d.atmost.iter().map(|m| m.0).collect();
        let clashes: Vec<&Vec<Ex>> = ax
            .disjoint
            .iter()
            .filter(|set| set.len() == 2 && set.iter().any(|e| holders.contains(e)))
            .collect();
        assert_eq!(clashes.len(), 1);
        assert!(clashes[0].contains(&a) && clashes[0].contains(&d.atmost[0].0));
    }

    #[test]
    fn minimums_of_one_strength_have_one_representative() {
        let (d, ax, mut it) = run_order(
            "SubClassOf(:A ObjectMinCardinality(3 :r :F))
             SubClassOf(:B ObjectMinCardinality(3 :r :F))
             SubClassOf(:C ObjectMinCardinality(3 :r :F))
             SubClassOf(:D ObjectMinCardinality(2 :r :F))",
            true,
        );
        let names: Vec<Ex> = ["A", "B", "C"]
            .iter()
            .map(|n| least_of(&d, &ax, &mut it, n))
            .collect();
        let weaker = least_of(&d, &ax, &mut it, "D");
        let first = *names.iter().min().unwrap();
        let witnesses = |g: Ex| d.least.iter().find(|l| l.0 == g).unwrap().1;
        let told = |g: Ex| -> Vec<Ex> {
            let mut v: Vec<Ex> = ax.subs.iter().filter(|p| p.0 == g).map(|p| p.1).collect();
            v.sort_unstable();
            v
        };
        for &g in &names {
            if g == first {
                assert!(!d.blockers.contains_key(&g));
                // the representative keeps its witnesses beside the names it is told
                // to be below
                let w = witnesses(g).unwrap();
                assert!(ax.subs.contains(&(g, w)));
            } else {
                assert_eq!(d.blockers.get(&g), Some(&vec![first]));
                assert!(ax.subs.contains(&(g, first)) && ax.subs.contains(&(first, g)));
                // the representative's witnesses are its own, under a clause as well
                assert_eq!(witnesses(g), None);
                assert_eq!(told(g), vec![first]);
            }
        }
        // the weaker minimum is blocked by the representative alone
        assert_eq!(d.blockers.get(&weaker), Some(&vec![first]));
        assert!(acyclic(&d.blockers));
    }

    /// What the ontology tells of `owl:Thing` is above every filler. A closure that
    /// followed it only from `owl:Thing` itself gave a relation that is not transitive,
    /// and the three minimums below blocked each other in a cycle.
    #[test]
    fn fillers_equivalent_through_owl_thing_are_ordered_by_name() {
        let (d, ax, mut it) = run_order(
            "SubClassOf(owl:Thing :B) SubClassOf(:B :A)
             SubClassOf(:X0 ObjectMinCardinality(2 :r :A))
             SubClassOf(:X1 ObjectMinCardinality(2 :r owl:Thing))
             SubClassOf(:X2 ObjectMinCardinality(2 :r :B))",
            true,
        );
        let mut names: Vec<Ex> = ["X0", "X1", "X2"]
            .iter()
            .map(|n| least_of(&d, &ax, &mut it, n))
            .collect();
        names.sort_unstable();
        assert!(acyclic(&d.blockers));
        assert!(!d.blockers.contains_key(&names[0]));
        assert_eq!(d.blockers.get(&names[1]), Some(&vec![names[0]]));
        assert_eq!(d.blockers.get(&names[2]), Some(&vec![names[0], names[1]]));
    }

    /// An axiom that the normalization refuses leaves no minimum in the order: the name
    /// that its first conjunct made goes with the axiom's other outputs, and the same
    /// minimum in a later axiom is ordered alone.
    #[test]
    fn a_refused_axiom_leaves_no_minimum_in_the_order() {
        let (taken, d, ax, mut it) = run_order_within(
            "SubClassOf(:A ObjectIntersectionOf(ObjectMinCardinality(2 :r :F)
               ObjectMinCardinality(65 :r :F)))
             SubClassOf(:B ObjectMinCardinality(2 :r :F))
             SubClassOf(:C ObjectMinCardinality(3 :r :F))",
            true,
            ORDER_RELATIONS,
        );
        assert_eq!(taken, vec![false, true, true, true, true]);
        assert_eq!(d.least.len(), 2);
        let (b, c) = (
            least_of(&d, &ax, &mut it, "B"),
            least_of(&d, &ax, &mut it, "C"),
        );
        assert_eq!(d.blockers.len(), 1);
        assert_eq!(d.blockers.get(&b), Some(&vec![c]));
        // the told inclusions into a minimum's name are those of the two classes and of
        // the larger minimum
        let names: Vec<Ex> = d.least.iter().map(|l| l.0).collect();
        let into: Vec<(Ex, Ex)> = ax
            .subs
            .iter()
            .copied()
            .filter(|(_, y)| names.contains(y))
            .collect();
        assert_eq!(into.len(), 3);
        assert!(into.contains(&(c, b)));
        assert!(d.least.iter().all(|l| l.1.is_some()));
    }

    /// Under any bound on its relations the order is a part of the whole one, with no
    /// cycle, and a minimum that is told equivalent to its representative has both
    /// inclusions and the representative as its blocker, or none of the three.
    #[test]
    fn a_bound_on_the_relations_keeps_a_part_of_the_order() {
        use std::collections::BTreeSet;
        let body = "SubClassOf(:F :G) SubObjectPropertyOf(:r :s)
             SubClassOf(:A1 ObjectMinCardinality(3 :r :F))
             SubClassOf(:A2 ObjectMinCardinality(3 :r :F))
             SubClassOf(:A3 ObjectMinCardinality(3 :r :F))
             SubClassOf(:B1 ObjectMinCardinality(2 :r :F))
             SubClassOf(:B2 ObjectMinCardinality(2 :r :F))
             SubClassOf(:C ObjectMinCardinality(2 :s :G))
             SubClassOf(:D ObjectMaxCardinality(1 :s :G))
             SubClassOf(:E ObjectMaxCardinality(2 :r :F))";
        // the inclusions, disjointness sets and blockers that a bound admits, beside those
        // of a run that admits none
        let (_, _, none, _) = run_order_within(body, true, 0);
        let base_subs: BTreeSet<(Ex, Ex)> = none.subs.iter().copied().collect();
        let base_sets: BTreeSet<Vec<Ex>> = none.disjoint.iter().cloned().collect();
        type Part = (
            BTreeSet<(Ex, Ex)>,
            BTreeSet<Vec<Ex>>,
            FxHashMap<Ex, Vec<Ex>>,
        );
        let part = |bound: usize| -> Part {
            let (taken, d, ax, _) = run_order_within(body, true, bound);
            assert!(taken.iter().all(|&t| t));
            assert_eq!(d.least.len(), 6);
            let subs: BTreeSet<(Ex, Ex)> = ax
                .subs
                .iter()
                .copied()
                .filter(|p| !base_subs.contains(p))
                .collect();
            // a name has witnesses of its own unless it is told below one of its blockers
            for &(g, w) in &d.least {
                let below = d
                    .blockers
                    .get(&g)
                    .is_some_and(|hs| hs.iter().any(|&h| subs.contains(&(g, h))));
                assert_eq!(w.is_none(), below, "bound {bound}");
                let own = base_subs
                    .iter()
                    .filter(|p| p.0 == g)
                    .filter(|p| ax.subs.contains(p))
                    .count();
                assert_eq!(own, usize::from(!below), "bound {bound}");
            }
            let sets = ax
                .disjoint
                .iter()
                .filter(|s| !base_sets.contains(*s))
                .cloned()
                .collect();
            (subs, sets, d.blockers)
        };
        let size = |p: &Part| p.0.len() + p.1.len() + p.2.values().map(Vec::len).sum::<usize>();
        let whole = part(ORDER_RELATIONS);
        // three names told equivalent to a representative, three pairs of
        // representatives, and nine pairs of a minimum and a smaller maximum
        assert_eq!(size(&whole), 24);
        assert!(part(0).0.is_empty() && part(0).1.is_empty() && part(0).2.is_empty());
        for bound in 0..=24 {
            let cut = part(bound);
            assert!(size(&cut) <= bound, "bound {bound}");
            assert!(cut.0.is_subset(&whole.0) && cut.1.is_subset(&whole.1));
            assert!(acyclic(&cut.2));
            for (g, hs) in &cut.2 {
                assert!(hs.iter().all(|h| whole.2[g].contains(h)));
            }
            for &(a, b) in &cut.0 {
                if whole.0.contains(&(b, a)) {
                    assert!(cut.0.contains(&(b, a)), "bound {bound}");
                    assert!(
                        cut.2.get(&a) == Some(&vec![b]) || cut.2.get(&b) == Some(&vec![a]),
                        "bound {bound}"
                    );
                }
            }
        }
        let all = part(24);
        assert!(all.0 == whole.0 && all.1 == whole.1 && all.2 == whole.2);
    }

    /// Two minimums over equivalent roles entail each other. The one with the smaller
    /// name blocks the other, which is told below it and so has no witnesses of its own.
    #[test]
    fn of_two_equivalent_minimums_the_larger_name_has_no_witnesses_of_its_own() {
        let (d, ax, mut it) = run_order(
            "EquivalentObjectProperties(:r :t)
             SubClassOf(:A ObjectMinCardinality(2 :r :F))
             SubClassOf(:B ObjectMinCardinality(2 :t :F))",
            true,
        );
        let (a, b) = (
            least_of(&d, &ax, &mut it, "A"),
            least_of(&d, &ax, &mut it, "B"),
        );
        let (first, second) = (a.min(b), a.max(b));
        assert!(ax.subs.contains(&(a, b)) && ax.subs.contains(&(b, a)));
        assert_eq!(d.blockers.get(&second), Some(&vec![first]));
        assert!(!d.blockers.contains_key(&first));
        let witnesses = |g: Ex| d.least.iter().find(|l| l.0 == g).unwrap().1;
        assert_eq!(witnesses(second), None);
        let w = witnesses(first).unwrap();
        assert!(ax.subs.contains(&(first, w)));
    }

    #[test]
    fn a_cycle_of_blockers_is_found() {
        let mut edges: FxHashMap<Ex, Vec<Ex>> = FxHashMap::default();
        edges.insert(1, vec![2]);
        edges.insert(2, vec![3, 4]);
        assert!(acyclic(&edges));
        edges.insert(4, vec![1]);
        assert!(!acyclic(&edges));
    }

    #[test]
    fn a_run_without_the_root_leaves_nominals_out() {
        let (t, d, _, _) =
            run(&[union(), AxiomKind::SubClassOf(c("D"), one_of(&["a", "b"]))]).unwrap();
        assert_eq!(t, vec![true, false]);
        assert!(d.root.is_none());
    }

    #[test]
    fn an_enumeration_on_the_right_is_a_disjunction_of_singletons() {
        let (t, d, _, it) =
            run_rooted(&[union(), AxiomKind::SubClassOf(c("D"), one_of(&["a", "b"]))]).unwrap();
        assert_eq!(t, vec![true, true]);
        assert!(d.root.is_some());
        let (_, h) = d
            .clauses
            .iter()
            .find(|(_, h)| h.iter().all(|&e| nominal(&it, e)))
            .expect("the clause of the enumeration");
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn an_enumeration_on_the_left_is_each_singleton_below() {
        let (t, _, ax, it) =
            run_rooted(&[union(), AxiomKind::SubClassOf(one_of(&["a", "b"]), c("D"))]).unwrap();
        assert_eq!(t, vec![true, true]);
        let below: Vec<Ex> = ax
            .subs
            .iter()
            .filter(|&&(a, _)| nominal(&it, a))
            .map(|&(_, y)| y)
            .collect();
        assert_eq!(below.len(), 2);
        assert_eq!(below[0], below[1]);
    }

    #[test]
    fn a_value_restriction_is_an_existential_into_a_singleton() {
        let (t, _, ax, it) = run_rooted(&[
            union(),
            AxiomKind::SubClassOf(c("D"), ClassExpression::ObjectHasValue(role("r"), ind("a"))),
            AxiomKind::SubClassOf(ClassExpression::ObjectHasValue(role("r"), ind("b")), c("E")),
        ])
        .unwrap();
        assert_eq!(t, vec![true, true, true]);
        let into = |e: Ex| matches!(it.nodes[e as usize], Node::Exist(_, f) if nominal(&it, f));
        assert!(ax.subs.iter().any(|&(_, e)| into(e)), "D ⊑ ∃r.{{a}}");
        assert!(ax.subs.iter().any(|&(e, _)| into(e)), "∃r.{{b}} ⊑ E");
    }

    #[test]
    fn a_minimum_above_one_without_counting_is_left_out_with_individuals() {
        let min = AxiomKind::SubClassOf(
            c("D"),
            ClassExpression::ObjectMinCardinality(2, role("r"), Some(Box::new(one_of(&["o"])))),
        );
        let (t, _, _, _) = run_rooted_counting(&[union(), min.clone()], false).unwrap();
        assert_eq!(t, vec![true, false]);
        // with counting allowed it alone turns counting on
        let (t, d, ax, _) = run_rooted_counting(&[union(), min.clone()], true).unwrap();
        assert_eq!(t, vec![true, true]);
        assert!(d.counting);
        assert_eq!(d.ne.len(), 1);
        assert!(ax.disjoint.is_empty());
        // and so does a maximum in a negative position, whose dual is a minimum of two
        let max = AxiomKind::SubClassOf(
            ClassExpression::ObjectMaxCardinality(1, role("r"), Some(Box::new(one_of(&["o"])))),
            c("D"),
        );
        let (t, d, _, _) = run_rooted_counting(&[union(), max], true).unwrap();
        assert_eq!(t, vec![true, true]);
        assert!(d.counting);
        // without individuals the minimum is read as the existential restriction
        let (t, _, _, _) = run(&[
            union(),
            AxiomKind::SubClassOf(
                c("D"),
                ClassExpression::ObjectMinCardinality(2, role("r"), Some(Box::new(c("E")))),
            ),
        ])
        .unwrap();
        assert_eq!(t, vec![true, true]);
    }

    #[test]
    fn a_minimum_with_counting_and_individuals_makes_its_successors_differ() {
        let (t, d, ax, _) = run_rooted(&[
            union(),
            AxiomKind::ClassAssertion(c("D"), ind("a")),
            AxiomKind::SubClassOf(
                c("D"),
                ClassExpression::ObjectMinCardinality(3, role("r"), Some(Box::new(c("E")))),
            ),
            AxiomKind::SubClassOf(
                c("F"),
                ClassExpression::ObjectMaxCardinality(1, role("s"), None),
            ),
        ])
        .unwrap();
        assert!(t[2] && t[3]);
        assert!(d.counting);
        assert_eq!(d.ne.len(), 1);
        assert_eq!(d.ne[0].1.len(), 3);
        assert!(ax.disjoint.is_empty(), "no fillers disjoint everywhere");
    }

    #[test]
    fn an_irreflexive_property_is_left_out_with_individuals() {
        let (t, _, _, _) = run_rooted(&[
            union(),
            AxiomKind::ClassAssertion(c("D"), ind("a")),
            AxiomKind::IrreflexiveObjectProperty(role("r")),
        ])
        .unwrap();
        assert!(t[0] && !t[2], "{t:?}");
    }

    #[test]
    fn a_nominal_an_inverse_and_a_maximum_together_take_nothing() {
        let with = |extra: AxiomKind| {
            run_rooted(&[
                union(),
                AxiomKind::ClassAssertion(c("D"), ind("a")),
                AxiomKind::SubClassOf(
                    c("F"),
                    ClassExpression::ObjectMaxCardinality(1, role("s"), None),
                ),
                extra,
            ])
        };
        let inv = ObjectPropertyExpression::InverseOf(ObjectProperty::new("http://ex.org/r"));
        assert!(
            with(AxiomKind::SubClassOf(
                c("G"),
                ClassExpression::ObjectSomeValuesFrom(inv, Box::new(c("H"))),
            ))
            .is_none()
        );
        assert!(with(AxiomKind::SymmetricObjectProperty(role("r"))).is_none());
        assert!(with(AxiomKind::InverseFunctionalObjectProperty(role("r"))).is_none());
        // without the inverse the maximum is taken
        assert!(with(AxiomKind::SubClassOf(c("G"), c("H"))).is_some());
    }

    #[test]
    fn assertions_outside_el_are_singleton_inclusions() {
        let inv = ObjectPropertyExpression::InverseOf(ObjectProperty::new("http://ex.org/r"));
        let union_bc =
            ClassExpression::ObjectUnionOf(std::collections::BTreeSet::from([c("B"), c("C")]));
        let (t, d, ax, it) = run_rooted(&[
            union(),
            AxiomKind::ClassAssertion(union_bc, ind("a")),
            AxiomKind::ObjectPropertyAssertion(inv.clone(), ind("a"), ind("b")),
            AxiomKind::NegativeObjectPropertyAssertion(inv, ind("b"), ind("c")),
        ])
        .unwrap();
        assert_eq!(t, vec![true, true, true, true]);
        // {a} ⊑ B ⊔ C
        assert!(d.clauses.iter().any(|(b, h)| {
            b.len() == 1 && nominal(&it, b[0]) && h.len() == 2 || {
                // or through a fresh class below {a}
                b.len() == 1
                    && ax.subs.iter().any(|&(n, y)| nominal(&it, n) && y == b[0])
                    && h.len() == 2
            }
        }));
        // {a} ⊑ ∃r⁻.{b}
        let inv_into = |e: Ex| matches!(it.nodes[e as usize], Node::Exist(r, f) if nominal(&it, f) && matches!(it.roles[r as usize], RoleName::Inverse(_)));
        assert!(ax.subs.iter().any(|&(n, e)| nominal(&it, n) && inv_into(e)));
        // {b} ⊓ ∃r⁻.{c} ⊑ ⊥
        assert!(
            ax.subs
                .iter()
                .any(|&(e, b)| b == BOTTOM && matches!(it.nodes[e as usize], Node::Conj(..)))
        );
    }

    #[test]
    fn a_union_on_the_right_is_a_clause() {
        let (t, d, _, it) = run(&[AxiomKind::SubClassOf(
            c("A"),
            ClassExpression::ObjectUnionOf(std::collections::BTreeSet::from([c("B"), c("C")])),
        )])
        .unwrap();
        assert_eq!(t, vec![true]);
        assert_eq!(d.clauses.len(), 1);
        let (b, h) = &d.clauses[0];
        assert_eq!(b.len(), 1);
        assert_eq!(h.len(), 2);
        assert!(matches!(it.nodes[b[0] as usize], Node::Class(_)));
    }

    #[test]
    fn a_complement_on_the_left_is_global() {
        let (_, d, _, _) = run(&[AxiomKind::SubClassOf(
            ClassExpression::ObjectComplementOf(Box::new(c("A"))),
            c("B"),
        )])
        .unwrap();
        assert_eq!(d.clauses.len(), 1);
        assert!(d.clauses[0].0.is_empty());
        assert_eq!(d.clauses[0].1.len(), 2);
    }

    #[test]
    fn a_universal_on_the_left_is_global_with_an_existential() {
        let r = ObjectPropertyExpression::Property(ObjectProperty::new("http://ex.org/r"));
        let (_, d, _, it) = run(&[AxiomKind::SubClassOf(
            ClassExpression::ObjectAllValuesFrom(r, Box::new(c("A"))),
            c("B"),
        )])
        .unwrap();
        assert_eq!(d.clauses.len(), 1);
        assert!(d.clauses[0].0.is_empty());
        assert!(
            d.clauses[0]
                .1
                .iter()
                .any(|&e| matches!(it.nodes[e as usize], Node::Exist(..)))
        );
    }

    #[test]
    fn a_universal_beside_a_class_on_the_left_keeps_the_class_as_body() {
        // D ⊓ ∀r.E ⊑ C is D → C ⊔ ∃r.¬E, a clause of D's contexts only (item 712)
        let r = ObjectPropertyExpression::Property(ObjectProperty::new("http://ex.org/r"));
        let (_, d, _, it) = run(&[AxiomKind::EquivalentClasses(
            std::collections::BTreeSet::from([
                c("C"),
                ClassExpression::ObjectIntersectionOf(std::collections::BTreeSet::from([
                    c("D"),
                    ClassExpression::ObjectAllValuesFrom(r.clone(), Box::new(c("E"))),
                ])),
            ]),
        )])
        .unwrap();
        assert!(!d.clauses.is_empty());
        for (b, h) in &d.clauses {
            assert!(!b.is_empty(), "a clause of every context");
            assert!(
                h.iter()
                    .any(|&e| matches!(it.nodes[e as usize], Node::Exist(..)))
            );
        }
        // inside an existential restriction on the left, the same with a fresh class
        let (_, d, _, _) = run(&[AxiomKind::SubClassOf(
            ClassExpression::ObjectSomeValuesFrom(
                r.clone(),
                Box::new(ClassExpression::ObjectIntersectionOf(
                    std::collections::BTreeSet::from([
                        c("D"),
                        ClassExpression::ObjectComplementOf(Box::new(c("E"))),
                    ]),
                )),
            ),
            c("C"),
        )])
        .unwrap();
        assert_eq!(d.clauses.len(), 1);
        assert!(!d.clauses[0].0.is_empty());
    }

    #[test]
    fn a_double_complement_on_the_right_is_its_class() {
        let (t, d, _, _) = run(&[AxiomKind::SubClassOf(
            c("A"),
            ClassExpression::ObjectUnionOf(std::collections::BTreeSet::from([
                c("B"),
                ClassExpression::ObjectComplementOf(Box::new(ClassExpression::ObjectComplementOf(
                    Box::new(c("C")),
                ))),
            ])),
        )])
        .unwrap();
        assert_eq!(t, vec![true]);
        assert_eq!(d.clauses.len(), 1);
        assert_eq!(d.clauses[0].1.len(), 2);
    }

    #[test]
    fn horn_axioms_alone_take_nothing() {
        assert!(run(&[AxiomKind::SubClassOf(c("A"), c("B"))]).is_none());
    }

    /// Phase 5 takes number restrictions: a maximum above zero becomes a fresh holder with
    /// an at-most record, and the run counts. A run that counts leaves out a minimum above
    /// 64, which a run without counting reads as an existential restriction.
    #[test]
    fn counting_is_taken_up_to_a_bound() {
        let r = ObjectPropertyExpression::Property(ObjectProperty::new("http://ex.org/r"));
        let (t, _, _, _) = run(&[
            AxiomKind::SubClassOf(
                c("A"),
                ClassExpression::ObjectUnionOf(std::collections::BTreeSet::from([c("B"), c("C")])),
            ),
            AxiomKind::SubClassOf(
                c("A"),
                ClassExpression::ObjectMaxCardinality(2, r, Some(Box::new(c("B")))),
            ),
        ])
        .unwrap();
        assert_eq!(t, vec![true, true]);
        let r = ObjectPropertyExpression::Property(ObjectProperty::new("http://ex.org/r"));
        let (t, _, _, _) = run(&[
            AxiomKind::SubClassOf(
                c("A"),
                ClassExpression::ObjectUnionOf(std::collections::BTreeSet::from([c("B"), c("C")])),
            ),
            AxiomKind::SubClassOf(
                c("A"),
                ClassExpression::ObjectMaxCardinality(2, r.clone(), Some(Box::new(c("B")))),
            ),
            AxiomKind::SubClassOf(
                c("A"),
                ClassExpression::ObjectMinCardinality(65, r, Some(Box::new(c("B")))),
            ),
        ])
        .unwrap();
        assert_eq!(t, vec![true, true, false]);
    }

    #[test]
    fn a_transitive_role_unfolds_its_universals() {
        let r = ObjectPropertyExpression::Property(ObjectProperty::new("http://ex.org/r"));
        let (_, _, ax, it) = run(&[
            AxiomKind::TransitiveObjectProperty(r.clone()),
            AxiomKind::SubClassOf(
                c("A"),
                ClassExpression::ObjectUnionOf(std::collections::BTreeSet::from([
                    c("B"),
                    ClassExpression::ObjectAllValuesFrom(r, Box::new(c("C"))),
                ])),
            ),
        ])
        .unwrap();
        // the universal's automaton: a state class with a universal back to itself
        let foralls = ax
            .subs
            .iter()
            .filter(|&&(_, x)| matches!(it.nodes[x as usize], Node::Forall(..)))
            .count();
        assert!(foralls >= 1, "{foralls}");
    }

    /// The states and transitions of the unfoldings of a run over functional-style axioms
    /// with the prefix `http://ex.org/` and a union, with the automata reduced to `level`,
    /// or as by default when it is `None`: the fresh classes of `X ⊑ ∀l.Y` with both fresh, and those axioms.
    fn unfolding(body: &str, level: Option<u8>) -> (usize, usize) {
        let doc = crate::ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n\
             SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n)"
        ))
        .unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        let mut allowed = Allowed::default();
        if let Some(level) = level {
            allowed.automata = level;
        }
        normalize(&kinds, &mut it, &mut ax, None, &|| false, allowed)
            .unwrap()
            .unwrap();
        let fresh = |e: Ex| matches!(it.nodes[e as usize], Node::Fresh(_));
        let mut states = FxHashSet::default();
        let mut transitions = 0;
        for &(x, a) in &ax.subs {
            if let Node::Forall(_, y) = it.nodes[a as usize]
                && fresh(x)
                && fresh(y)
            {
                states.insert(x);
                states.insert(y);
                transitions += 1;
            }
        }
        (states.len(), transitions)
    }

    /// A transitive property under a transitive property, as BioTop's parthood has: the
    /// closure leaves the transitions of the subproperty beside those of the property and
    /// states that accept the same words, and the run unfolds by two states and two
    /// transitions after the reductions (roadmap item 712).
    #[test]
    fn tier_d_unfolds_a_universal_by_the_reduced_automaton() {
        let body = "TransitiveObjectProperty(:r)\nTransitiveObjectProperty(:s)\n\
                    SubObjectPropertyOf(:s :r)\nSubClassOf(:A ObjectAllValuesFrom(:r :B))";
        let levels: Vec<(usize, usize)> = (0..3).map(|l| unfolding(body, Some(l))).collect();
        assert_eq!(levels, [(3, 6), (2, 4), (2, 2)]);
        assert_eq!(unfolding(body, None), (2, 2));
    }

    #[test]
    fn repeated_property_pairs_do_not_expand_quadratically_in_dl_norm() {
        let p = ObjectPropertyExpression::Property(ObjectProperty::new("http://ex.org/p"));
        let q = ObjectPropertyExpression::Property(ObjectProperty::new("http://ex.org/q"));
        let disj = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("http://ex.org/A")),
            ClassExpression::ObjectUnionOf(
                [
                    ClassExpression::Class(Class::new("http://ex.org/B")),
                    ClassExpression::Class(Class::new("http://ex.org/C")),
                ]
                .into(),
            ),
        );
        let allowed = Allowed {
            counting: true,
            ..Allowed::default()
        };

        let same = AxiomKind::DisjointObjectProperties(vec![p.clone(); 500].into());
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        let (taken, dl) = normalize(&[&disj, &same], &mut it, &mut ax, None, &|| false, allowed)
            .unwrap()
            .unwrap();
        assert_eq!(taken, vec![true, true]);
        assert_eq!(dl.rdis.len(), 1);

        let mut mix_vec = vec![p; 500];
        mix_vec.push(q);
        let mix = AxiomKind::DisjointObjectProperties(mix_vec.into());
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        let (taken, dl) = normalize(&[&disj, &mix], &mut it, &mut ax, None, &|| false, allowed)
            .unwrap()
            .unwrap();
        assert_eq!(taken, vec![true, true]);
        assert_eq!(dl.rdis.len(), 2);
    }

    #[test]
    fn role_pair_normalization_enforces_post_interning_budget_and_cancellation_in_dl_norm() {
        use std::cell::Cell;

        let disj = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("http://ex.org/A")),
            ClassExpression::ObjectUnionOf(
                [
                    ClassExpression::Class(Class::new("http://ex.org/B")),
                    ClassExpression::Class(Class::new("http://ex.org/C")),
                ]
                .into(),
            ),
        );
        let allowed = Allowed {
            counting: true,
            ..Allowed::default()
        };

        // 1. Stale pre-interning baseline gap in Tier D role pass.
        let distinct_400: Vec<ObjectPropertyExpression> = (0..400)
            .map(|i| {
                ObjectPropertyExpression::Property(ObjectProperty::new(format!(
                    "http://ex.org/dl_prop_{i:04}"
                )))
            })
            .collect();
        let disjoint_400 = AxiomKind::DisjointObjectProperties(distinct_400.into());
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        it.scope_axiom(&disjoint_400);
        let pre_baseline = it.bytes() + ax.bytes();
        let pair_bytes = (400usize * 399 / 2) * std::mem::size_of::<(Role, Role)>();
        let tight_limit = pre_baseline + pair_bytes + 1024;
        let err = normalize(
            &[&disj, &disjoint_400],
            &mut it,
            &mut ax,
            Some(tight_limit),
            &|| false,
            allowed,
        );
        assert!(
            matches!(err, Err(Halt::Memory(_))),
            "expected Halt::Memory in dl_norm when post-interning + scratch + pairs exceed limit, got {:?}",
            err.err()
        );

        let mut it_ok = Interner::new();
        let mut ax_ok = Axioms::default();
        let (taken, dl) = normalize(
            &[&disj, &disjoint_400],
            &mut it_ok,
            &mut ax_ok,
            Some(pre_baseline + pair_bytes + 512 * 1024),
            &|| false,
            allowed,
        )
        .unwrap()
        .unwrap();
        assert_eq!(taken, vec![true, true]);
        assert_eq!(dl.rdis.len(), 400 * 399 / 2);

        // 2. Cancellation before reserve in Tier D.
        let distinct_20: Vec<ObjectPropertyExpression> = (0..20)
            .map(|i| {
                ObjectPropertyExpression::Property(ObjectProperty::new(format!(
                    "http://ex.org/dl_cancel_{i}"
                )))
            })
            .collect();
        let disjoint_20 = AxiomKind::DisjointObjectProperties(distinct_20.into());
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        let err = normalize(
            &[&disj, &disjoint_20],
            &mut it,
            &mut ax,
            None,
            &|| true,
            allowed,
        );
        assert!(
            matches!(err, Err(Halt::Stopped)),
            "expected Halt::Stopped before reserve in dl_norm, got {:?}",
            err.err()
        );

        // 3. Cancellation during duplicate operand scan of > POLL_WORK copies of a single role.
        let p = ObjectPropertyExpression::Property(ObjectProperty::new("http://ex.org/dl_dup_p"));
        let many_same = AxiomKind::DisjointObjectProperties(
            vec![p; super::super::index::POLL_WORK + 10].into(),
        );
        let polls = Cell::new(0usize);
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        let err = normalize(
            &[&disj, &many_same],
            &mut it,
            &mut ax,
            None,
            &|| {
                polls.set(polls.get() + 1);
                true
            },
            allowed,
        );
        assert!(
            matches!(err, Err(Halt::Stopped)),
            "expected Halt::Stopped during duplicate scan in dl_norm, got {:?}",
            err.err()
        );
        assert!(polls.get() >= 1, "expected stop() to be polled during scan");
    }

    #[test]
    fn role_pair_aggregate_growth_respects_memory_budget_in_dl_norm() {
        let disj = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("http://ex.org/A")),
            ClassExpression::ObjectUnionOf(
                [
                    ClassExpression::Class(Class::new("http://ex.org/B")),
                    ClassExpression::Class(Class::new("http://ex.org/C")),
                ]
                .into(),
            ),
        );
        let allowed = Allowed {
            counting: true,
            ..Allowed::default()
        };

        let distinct_400: Vec<ObjectPropertyExpression> = (0..400)
            .map(|i| {
                ObjectPropertyExpression::Property(ObjectProperty::new(format!(
                    "http://ex.org/dl_agg_{i:04}"
                )))
            })
            .collect();
        let disjoint_400 = AxiomKind::DisjointObjectProperties(distinct_400.clone().into());
        let dup_first = AxiomKind::DisjointObjectProperties(
            vec![distinct_400[0].clone(), distinct_400[0].clone()].into(),
        );
        let disjoint_200 = AxiomKind::DisjointObjectProperties(distinct_400[..200].to_vec().into());

        let mut it = Interner::new();
        let mut ax = Axioms::default();
        let pre_baseline = it.bytes() + ax.bytes();
        let pair_bytes = (400usize * 399 / 2) * std::mem::size_of::<(Role, Role)>();

        // 1. Extending `all.rdis` from 79,800 to 159,600 slots (~1.22 MiB) when adding 1 pair
        // must fail under a budget that fits the first axiom + overhead (~894 KiB) but not the
        // doubled aggregate buffer.
        let growth_limit = pre_baseline + pair_bytes + 256 * 1024;
        let err = normalize(
            &[&disj, &disjoint_400, &dup_first],
            &mut it,
            &mut ax,
            Some(growth_limit),
            &|| false,
            allowed,
        );
        assert!(
            matches!(err, Err(Halt::Memory(_))),
            "expected Halt::Memory when aggregate rdis growth exceeds limit, got {:?}",
            err.err()
        );

        // 2. When the limit allows the doubled aggregate (2 * pair_bytes) + 128 KiB headroom,
        // a third axiom emitting 19,900 pairs (159,200 bytes) must see the retained 159,600-slot
        // capacity of `all.rdis` and fail rather than charging only 79,801 slots.
        let mut it2 = Interner::new();
        let mut ax2 = Axioms::default();
        let retained_limit = pre_baseline + 2 * pair_bytes + 128 * 1024;
        let err2 = normalize(
            &[&disj, &disjoint_400, &dup_first, &disjoint_200],
            &mut it2,
            &mut ax2,
            Some(retained_limit),
            &|| false,
            allowed,
        );
        assert!(
            matches!(err2, Err(Halt::Memory(_))),
            "expected Halt::Memory when third axiom exceeds budget over retained aggregate capacity, got {:?}",
            err2.err()
        );

        // 3. With enough headroom for the retained aggregate + third axiom buffer, all succeed.
        let mut it_ok = Interner::new();
        let mut ax_ok = Axioms::default();
        let (taken, dl) = normalize(
            &[&disj, &disjoint_400, &dup_first, &disjoint_200],
            &mut it_ok,
            &mut ax_ok,
            Some(pre_baseline + 3 * pair_bytes + 512 * 1024),
            &|| false,
            allowed,
        )
        .unwrap()
        .unwrap();
        assert_eq!(taken, vec![true, true, true, true]);
        assert_eq!(dl.rdis.len(), 400 * 399 / 2 + 1 + 200 * 199 / 2);
    }
}
