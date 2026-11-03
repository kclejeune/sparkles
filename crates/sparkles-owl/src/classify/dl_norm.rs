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
use super::index::{Axioms, BOTTOM, Ex, Halt, Interner, NO_ROLE, Node, Role, RoleName, TOP};
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
    /// the axioms outside OWL 2 EL the run takes, and how many of them need case analysis
    pub taken: usize,
    pub disjunctive: usize,
}

impl DlAx {
    pub fn bytes(&self) -> usize {
        self.clauses
            .iter()
            .map(|(b, h)| 48 + (b.capacity() + h.capacity()) * 4)
            .sum::<usize>()
            + super::saturate::table_bytes(self.hyp.capacity(), 8)
    }
}

/// Why an axiom stays left out.
type Why = &'static str;

/// Whether the EL part of a run lets Tier D take the axioms outside OWL 2 EL: no
/// individuals, top or bottom properties, data properties, self restrictions or reflexive
/// roles, whose rules this phase does not have.
pub fn eligible(it: &Interner, ax: &Axioms) -> bool {
    it.individuals.is_empty()
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
        && !it.nodes.iter().any(|n| {
            matches!(
                n,
                Node::SelfLoop(_) | Node::Nominal(_) | Node::DataSome(..) | Node::AtMost1(..)
            )
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
}

impl Out {
    fn extend(&mut self, o: Out) {
        self.subs.extend(o.subs);
        self.disjoint.extend(o.disjoint);
        self.ranges.extend(o.ranges);
        self.role_subs.extend(o.role_subs);
        self.chains.extend(o.chains);
        self.clauses.extend(o.clauses);
    }
}

struct Norm<'a> {
    it: &'a mut Interner,
    out: Out,
}

impl Norm<'_> {
    /// The role of an object property expression, with its inverse interned beside it.
    fn role(&mut self, p: &ObjectPropertyExpression) -> Result<Role, Why> {
        let named = p.named();
        if matches!(
            named.as_str(),
            vocab::OWL_TOP_OBJECT_PROPERTY | vocab::OWL_BOTTOM_OBJECT_PROPERTY
        ) {
            return Err("the top and bottom object properties");
        }
        let r = self.it.role(RoleName::Object(named.clone()));
        let i = self.it.role(RoleName::Inverse(named.clone()));
        Ok(match p {
            ObjectPropertyExpression::Property(_) => r,
            ObjectPropertyExpression::InverseOf(_) => i,
        })
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
            C::ObjectMaxCardinality(..) | C::ObjectExactCardinality(..) => {
                return Err("a maximum cardinality above zero in a positive position");
            }
            C::ObjectOneOf(_) | C::ObjectHasValue(..) => return Err("individuals"),
            C::ObjectHasSelf(_) => return Err("self restrictions"),
            _ => return Err("data restrictions"),
        })
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

    /// `¬f` in a positive position: `x` with `x ⊓ y ⊑ ⊥` for f's negative form y.
    fn not(&mut self, f: &ClassExpression, d: usize) -> Result<Ex, Why> {
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
                let es = cs
                    .iter()
                    .map(|c| self.neg(c, d))
                    .collect::<Result<_, _>>()?;
                self.conj(es)
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
            C::ObjectMaxCardinality(_, p, f) | C::ObjectExactCardinality(0, p, f) => {
                // ≤n p.f ⊑ y is ⊤ ⊑ y ⊔ ≥(n+1) p.f, taken as ⊤ ⊑ y ⊔ ∃p.f; =0 is ≤0
                let pf = match f {
                    Some(f) => self.pos(f, d)?,
                    None => TOP,
                };
                if pf == BOTTOM {
                    return Ok(TOP);
                }
                let r = self.role(p)?;
                let e = self.it.node(Node::Exist(r, pf));
                let y = self.it.fresh_class();
                self.out.clauses.push((Vec::new(), vec![e, y]));
                y
            }
            C::ObjectExactCardinality(..) => {
                return Err("an exact cardinality in a negative position");
            }
            C::ObjectOneOf(_) | C::ObjectHasValue(..) => return Err("individuals"),
            C::ObjectHasSelf(_) => return Err("self restrictions"),
            _ => return Err("data restrictions"),
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
                    C::ObjectComplementOf(f) => todo_r.push(*f),
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
                let cs: Vec<&ClassExpression> = cs.iter().collect();
                let mut es = cs
                    .iter()
                    .map(|c| self.neg(c, 0))
                    .collect::<Result<Vec<_>, _>>()?;
                es.sort_unstable();
                for w in es.windows(2) {
                    if w[0] == w[1] {
                        self.out.subs.push((w[0], BOTTOM));
                    }
                }
                es.dedup();
                es.retain(|&e| e != BOTTOM);
                if es.len() >= 2 {
                    self.out.disjoint.push(es);
                }
                Ok(())
            }
            K::DisjointUnion(c, ds) => {
                let c = C::Class(c.clone());
                let ds: Vec<ClassExpression> = ds.iter().cloned().collect();
                self.sub(vec![c.clone()], ds.clone())?;
                for d in &ds {
                    self.sub(vec![d.clone()], vec![c.clone()])?;
                }
                self.axiom(&K::DisjointClasses(ds.into_iter().collect()))
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
            _ => Err("an axiom Tier D does not take"),
        }
    }
}

/// The normalization of a Tier D run: the axioms outside OWL 2 EL that it takes, with the
/// clauses of case analysis, after the EL axioms are in `ax`. `None` when it takes no axiom
/// that needs case analysis, or when the role inclusions are not regular or their automata
/// pass the limit, and then `ax` is as it was, the interner aside. The answer's flags say
/// which axioms were taken.
pub fn normalize(
    axioms: &[&AxiomKind],
    it: &mut Interner,
    ax: &mut Axioms,
    limit: Option<usize>,
    stop: &dyn Fn() -> bool,
) -> Result<Option<(Vec<bool>, DlAx)>, Halt> {
    let mut taken = vec![false; axioms.len()];
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
    for (i, k) in axioms.iter().enumerate() {
        if !Norm::is_role_axiom(k) {
            continue;
        }
        let mut n = Norm {
            it,
            out: Out::default(),
        };
        if n.role_axiom(k).is_ok() {
            all.extend(n.out);
            taken[i] = true;
        }
    }
    let mut disjunctive = 0;
    for (i, k) in axioms.iter().enumerate() {
        if taken[i] || Norm::is_role_axiom(k) {
            continue;
        }
        let mut n = Norm {
            it,
            out: Out::default(),
        };
        if n.axiom(k).is_ok() {
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
    unfold(it, ax, &all.clauses, &mut rb);
    let mut dl = DlAx {
        clauses: all.clauses,
        taken: taken.iter().filter(|t| **t).count(),
        disjunctive,
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
        let h = it.fresh_class();
        dl.hyp.insert(e, h);
    }
    Ok(Some((taken, dl)))
}

/// The universals on roles that are not simple, unfolded by the roles' automata: those that
/// occur positively, and those that the negative existential restrictions over such a role
/// stand for, `∃s.F ⊑ e` being `F ⊑ ∀s⁻.e`. Each state of an automaton without
/// ε-transitions is a fresh class: `B ⊑ X_start`, `X_p ⊑ ∀l.X_q` for each transition and
/// `X_final ⊑ C`. The universal itself stays, which is sound on the direct links.
fn unfold(it: &mut Interner, ax: &mut Axioms, clauses: &[(Vec<Ex>, Vec<Ex>)], rb: &mut RoleBox) {
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
    for (holder, r, f) in unfolds {
        let Some(nfa) = rb.auto(r, &mut Vec::new()) else {
            continue;
        };
        let a = autos.entry(r).or_insert_with(|| nfa.eps_free());
        let states: Vec<Ex> = (0..a.states).map(|_| it.fresh_class()).collect();
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
        normalize(&refs, &mut it, &mut ax, None, &|| false)
            .unwrap()
            .map(|(t, d)| (t, d, ax, it))
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
    fn horn_axioms_alone_take_nothing() {
        assert!(run(&[AxiomKind::SubClassOf(c("A"), c("B"))]).is_none());
    }

    #[test]
    fn counting_stays_out() {
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
        assert_eq!(t, vec![true, false]);
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
}
