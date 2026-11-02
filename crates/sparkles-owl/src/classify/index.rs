//! The indexed form of an EL ontology: interned class expressions, the rules that each
//! expression triggers when a context derives it, and the role table.
//!
//! Class expressions are interned once as [`Node`]s and named by an [`Ex`]. An
//! intersection is binary, nested in the order of its operands' ids, as in ELK
//! (Kazakov, Krötzsch and Simančík, JAR 2014, §4). The [`Index`] keeps, for each
//! expression, its told superexpressions, the negative intersections it is an operand of,
//! the negative existential restrictions it is the filler of and the disjointness axioms
//! it is a member of. The index is rebuilt from the ontology's axioms on each run, which is
//! linear in their number, while the interner keeps its ids across runs so that an
//! incremental run can keep the contexts it does not touch.

use crate::model::*;
use crate::vocab;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::Cell;
use std::collections::BTreeSet;

/// An interned class expression.
pub type Ex = u32;
/// An interned role: an object property, a data property or a role the chain
/// normalization introduces.
pub type Role = u32;

pub const TOP: Ex = 0;
pub const BOTTOM: Ex = 1;

/// One interned class expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Top,
    Bottom,
    /// a named class, by its index in [`Interner::classes`]
    Class(u32),
    /// `ObjectOneOf(a)`, by the individual's index
    Nominal(u32),
    /// a data range or a literal, by its index in [`Interner::data`], used as an atom
    Data(u32),
    Conj(Ex, Ex),
    Exist(Role, Ex),
    SelfLoop(Role),
}

/// The name of a role.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RoleName {
    Object(ObjectProperty),
    Data(DataProperty),
    /// a role the binarization of a property chain introduces
    Fresh(u32),
}

/// The atom a data range stands for: a range, or one literal of `DataHasValue`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DataAtom {
    Range(DataRange),
    Value(Literal),
}

/// The interned expressions and the names they use. Ids are never reused, so an
/// incremental run can keep contexts by id.
#[derive(Default)]
pub struct Interner {
    pub nodes: Vec<Node>,
    map: FxHashMap<Node, Ex>,
    pub classes: Vec<Class>,
    class_ids: FxHashMap<Class, u32>,
    pub individuals: Vec<NamedIndividual>,
    individual_ids: FxHashMap<NamedIndividual, u32>,
    pub data: Vec<DataAtom>,
    data_ids: FxHashMap<DataAtom, u32>,
    pub roles: Vec<RoleName>,
    role_ids: FxHashMap<RoleName, Role>,
    fresh: u32,
}

impl Interner {
    pub fn new() -> Interner {
        let mut i = Interner::default();
        i.node(Node::Top);
        i.node(Node::Bottom);
        i
    }

    /// The role of a name, if the interner has it.
    pub fn role_id(&self, name: &RoleName) -> Option<Role> {
        self.role_ids.get(name).copied()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn node(&mut self, n: Node) -> Ex {
        if let Some(&e) = self.map.get(&n) {
            return e;
        }
        let e = self.nodes.len() as Ex;
        self.nodes.push(n);
        self.map.insert(n, e);
        e
    }

    pub fn get(&self, n: &Node) -> Option<Ex> {
        self.map.get(n).copied()
    }

    pub fn class(&mut self, c: &Class) -> Ex {
        match c.as_str() {
            vocab::OWL_THING => return TOP,
            vocab::OWL_NOTHING => return BOTTOM,
            _ => {}
        }
        let id = match self.class_ids.get(c) {
            Some(&id) => id,
            None => {
                let id = self.classes.len() as u32;
                self.classes.push(c.clone());
                self.class_ids.insert(c.clone(), id);
                id
            }
        };
        self.node(Node::Class(id))
    }

    /// The expression of a named class, when it has been interned.
    pub fn class_ex(&self, c: &Class) -> Option<Ex> {
        match c.as_str() {
            vocab::OWL_THING => Some(TOP),
            vocab::OWL_NOTHING => Some(BOTTOM),
            _ => self
                .class_ids
                .get(c)
                .and_then(|&id| self.get(&Node::Class(id))),
        }
    }

    pub fn nominal(&mut self, i: &NamedIndividual) -> Ex {
        let id = match self.individual_ids.get(i) {
            Some(&id) => id,
            None => {
                let id = self.individuals.len() as u32;
                self.individuals.push(i.clone());
                self.individual_ids.insert(i.clone(), id);
                id
            }
        };
        self.node(Node::Nominal(id))
    }

    pub fn data(&mut self, d: DataAtom) -> Ex {
        let id = match self.data_ids.get(&d) {
            Some(&id) => id,
            None => {
                let id = self.data.len() as u32;
                self.data.push(d.clone());
                self.data_ids.insert(d, id);
                id
            }
        };
        self.node(Node::Data(id))
    }

    pub fn data_ex(&self, d: &DataAtom) -> Option<Ex> {
        self.data_ids
            .get(d)
            .and_then(|&id| self.get(&Node::Data(id)))
    }

    pub fn role(&mut self, r: RoleName) -> Role {
        if let Some(&id) = self.role_ids.get(&r) {
            return id;
        }
        let id = self.roles.len() as Role;
        self.roles.push(r.clone());
        self.role_ids.insert(r, id);
        id
    }

    pub fn fresh_role(&mut self) -> Role {
        self.fresh += 1;
        self.role(RoleName::Fresh(self.fresh))
    }

    /// The binary intersection of two expressions, with its operands ordered.
    pub fn conj(&mut self, a: Ex, b: Ex) -> Ex {
        if a == b {
            return a;
        }
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        self.node(Node::Conj(a, b))
    }
}

/// The rules of the role hierarchy, the chains and the ranges.
#[derive(Default)]
pub struct Roles {
    /// for each role, its superroles with itself, sorted
    pub sup: Vec<Vec<Role>>,
    /// for each pair of roles, the roles a chain composes them into
    pub comp: FxHashMap<(Role, Role), Vec<Role>>,
    /// roles that are the second operand of a chain through a superrole, whose links a
    /// context keeps as forward links
    pub right: Vec<bool>,
    /// roles that are the first operand of a chain through a superrole
    pub left: Vec<bool>,
    /// the reflexive roles, with their superroles
    pub reflexive: Vec<Role>,
    /// for each role, the intersection of its ranges and those of its superroles
    pub range: Vec<Option<Ex>>,
}

impl Roles {
    /// The bytes the tables take, roughly.
    pub fn bytes(&self) -> usize {
        self.sup
            .iter()
            .map(|v| 24 + v.capacity() * 4)
            .sum::<usize>()
            + self
                .comp
                .values()
                .map(|v| 40 + v.capacity() * 4)
                .sum::<usize>()
            + self.right.len()
            + self.left.len()
            + self.reflexive.len() * 4
            + self.range.len() * 8
    }

    /// Whether `r` is a subrole of `s`, itself included.
    #[inline]
    pub fn sub(&self, r: Role, s: Role) -> bool {
        r == s
            || self
                .sup
                .get(r as usize)
                .is_some_and(|v| v.binary_search(&s).is_ok())
    }

    pub fn comp(&self, r1: Role, r2: Role) -> &[Role] {
        if !self.left.get(r1 as usize).copied().unwrap_or(false) {
            return &[];
        }
        self.comp.get(&(r1, r2)).map_or(&[], |v| v.as_slice())
    }

    #[inline]
    pub fn keeps_forward(&self, r: Role) -> bool {
        self.right.get(r as usize).copied().unwrap_or(false)
    }
}

/// Why an axiom takes part only in part, or not at all.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Gap {
    /// The axiom is outside OWL 2 EL and is left out.
    Outside { rule: String, message: String },
    /// The classifier reads the axiom but may miss some of its consequences.
    Partial(&'static str),
}

/// The normalized logical content of the EL axioms.
#[derive(Default)]
pub struct Axioms {
    /// `sub ⊑ sup`
    pub subs: Vec<(Ex, Ex)>,
    /// disjoint sets of at least two expressions
    pub disjoint: Vec<Vec<Ex>>,
    /// `r ⊑ s`
    pub role_subs: Vec<(Role, Role)>,
    /// `r1 ∘ … ∘ rn ⊑ s`, n ≥ 2
    pub chains: Vec<(Vec<Role>, Role)>,
    pub reflexive: Vec<Role>,
    /// `range(r) = c`
    pub ranges: Vec<(Role, Ex)>,
    /// the individuals of assertions, which classification checks for consistency
    pub individuals: Vec<Ex>,
    /// the classes of the signature
    pub classes: Vec<Ex>,
}

/// Maps one axiom's logical content into the interner. `out` collects what it means,
/// and the answer says what of it the classifier may not fully take into account. An
/// axiom with an expression the classifier cannot represent adds nothing to `out`.
pub fn normalize(ax: &AxiomKind, it: &mut Interner, out: &mut Axioms) -> Option<&'static str> {
    match normalize_axiom(ax, it, out) {
        Ok(partial) => partial,
        Err(why) => Some(why),
    }
}

/// [`normalize`], with an error for an axiom left out. Every branch interns all its
/// expressions before it adds to `out`, so that an error leaves `out` as it was.
fn normalize_axiom(
    ax: &AxiomKind,
    it: &mut Interner,
    out: &mut Axioms,
) -> Result<Option<&'static str>, &'static str> {
    use AxiomKind as K;
    let mut partial: Option<&'static str> = None;
    fn ce(
        c: &ClassExpression,
        it: &mut Interner,
        partial: &mut Option<&'static str>,
    ) -> Result<Ex, &'static str> {
        let (e, p) = class_expression(c, it, 0)?;
        if partial.is_none() {
            *partial = p;
        }
        Ok(e)
    }
    const BUILTIN: &str = "the top and bottom object and data properties";
    match ax {
        K::Declaration(Entity::Class(c)) => {
            let e = it.class(c);
            out.classes.push(e);
        }
        K::Declaration(_) => {}
        K::SubClassOf(a, b) => {
            let a = ce(a, it, &mut partial)?;
            let b = ce(b, it, &mut partial)?;
            out.subs.push((a, b));
        }
        K::EquivalentClasses(cs) => {
            let es: Vec<Ex> = cs
                .iter()
                .map(|c| ce(c, it, &mut partial))
                .collect::<Result<_, _>>()?;
            for w in 0..es.len() {
                let next = es[(w + 1) % es.len()];
                if es[w] != next {
                    out.subs.push((es[w], next));
                }
            }
        }
        K::DisjointClasses(cs) => {
            let mut es: Vec<Ex> = cs
                .iter()
                .map(|c| ce(c, it, &mut partial))
                .collect::<Result<_, _>>()?;
            es.sort_unstable();
            let before = es.len();
            es.dedup();
            if es.len() < before {
                // two operands that normalize to one expression make it unsatisfiable
                let mut seen = FxHashSet::default();
                for c in cs {
                    let e = ce(c, it, &mut partial)?;
                    if !seen.insert(e) {
                        out.subs.push((e, BOTTOM));
                    }
                }
            }
            if es.len() >= 2 {
                out.disjoint.push(es);
            }
        }
        K::SubObjectPropertyOf(sub, sup) => {
            let Some(s) = object_role(sup, it) else {
                return Err("inverse properties");
            };
            match sub {
                SubObjectPropertyExpression::Expression(p) => {
                    let Some(r) = object_role(p, it) else {
                        return Err("inverse properties");
                    };
                    out.role_subs.push((r, s));
                }
                SubObjectPropertyExpression::Chain(ps) => {
                    let mut rs = Vec::with_capacity(ps.len());
                    for p in ps {
                        let Some(r) = object_role(p, it) else {
                            return Err("inverse properties");
                        };
                        rs.push(r);
                    }
                    match rs.len() {
                        0 => {}
                        1 => out.role_subs.push((rs[0], s)),
                        _ => out.chains.push((rs, s)),
                    }
                }
            }
            if is_top_or_bottom_role(sup) || is_top_or_bottom_sub(sub) {
                partial = Some(BUILTIN);
            }
        }
        K::EquivalentObjectProperties(ps) => {
            let rs: Vec<Role> = ps.iter().filter_map(|p| object_role(p, it)).collect();
            for w in 0..rs.len() {
                let next = rs[(w + 1) % rs.len()];
                if rs[w] != next {
                    out.role_subs.push((rs[w], next));
                }
            }
            if ps.iter().any(is_top_or_bottom_role) {
                partial = Some(BUILTIN);
            }
        }
        K::TransitiveObjectProperty(p) => {
            if let Some(r) = object_role(p, it) {
                out.chains.push((vec![r, r], r));
            }
            if is_top_or_bottom_role(p) {
                partial = Some(BUILTIN);
            }
        }
        K::ReflexiveObjectProperty(p) => {
            if let Some(r) = object_role(p, it) {
                out.reflexive.push(r);
            }
            if is_top_or_bottom_role(p) {
                partial = Some(BUILTIN);
            }
        }
        K::ObjectPropertyDomain(p, c) => {
            if let Some(r) = object_role(p, it) {
                let c = ce(c, it, &mut partial)?;
                let e = it.node(Node::Exist(r, TOP));
                out.subs.push((e, c));
            }
            if is_top_or_bottom_role(p) {
                partial = Some(BUILTIN);
            }
        }
        K::ObjectPropertyRange(p, c) => {
            if let Some(r) = object_role(p, it) {
                let c = ce(c, it, &mut partial)?;
                out.ranges.push((r, c));
            }
            if is_top_or_bottom_role(p) {
                partial = Some(BUILTIN);
            }
        }
        K::SubDataPropertyOf(a, b) => {
            let a = it.role(RoleName::Data(a.clone()));
            let b = it.role(RoleName::Data(b.clone()));
            out.role_subs.push((a, b));
        }
        K::EquivalentDataProperties(ps) => {
            let rs: Vec<Role> = ps
                .iter()
                .map(|p| it.role(RoleName::Data(p.clone())))
                .collect();
            for w in 0..rs.len() {
                let next = rs[(w + 1) % rs.len()];
                if rs[w] != next {
                    out.role_subs.push((rs[w], next));
                }
            }
        }
        K::DataPropertyDomain(d, c) => {
            let c = ce(c, it, &mut partial)?;
            let r = it.role(RoleName::Data(d.clone()));
            let top = it.data(DataAtom::Range(DataRange::Datatype(Datatype::literal())));
            let e = it.node(Node::Exist(r, top));
            out.subs.push((e, c));
        }
        K::ClassAssertion(c, Individual::Named(i)) => {
            let c = ce(c, it, &mut partial)?;
            let n = it.nominal(i);
            out.individuals.push(n);
            out.subs.push((n, c));
        }
        K::ObjectPropertyAssertion(p, Individual::Named(a), Individual::Named(b)) => {
            if let Some(r) = object_role(p, it) {
                let a = it.nominal(a);
                let b = it.nominal(b);
                let e = it.node(Node::Exist(r, b));
                out.individuals.push(a);
                out.individuals.push(b);
                out.subs.push((a, e));
            }
            if is_top_or_bottom_role(p) {
                partial = Some(BUILTIN);
            }
        }
        K::DataPropertyAssertion(d, Individual::Named(a), v) => {
            let r = it.role(RoleName::Data(d.clone()));
            let a = it.nominal(a);
            let v = it.data(DataAtom::Value(v.clone()));
            let e = it.node(Node::Exist(r, v));
            out.individuals.push(a);
            out.subs.push((a, e));
            partial = Some("data values");
        }
        K::SameIndividual(is) => {
            let es: Vec<Ex> = is
                .iter()
                .filter_map(|i| match i {
                    Individual::Named(n) => Some(it.nominal(n)),
                    Individual::Anonymous(_) => None,
                })
                .collect();
            out.individuals.extend(es.iter().copied());
            for w in 0..es.len() {
                let next = es[(w + 1) % es.len()];
                if es[w] != next {
                    out.subs.push((es[w], next));
                }
            }
            partial = Some("individuals");
        }
        K::DifferentIndividuals(is) => {
            let mut es: Vec<Ex> = is
                .iter()
                .filter_map(|i| match i {
                    Individual::Named(n) => Some(it.nominal(n)),
                    Individual::Anonymous(_) => None,
                })
                .collect();
            out.individuals.extend(es.iter().copied());
            es.sort_unstable();
            es.dedup();
            if es.len() >= 2 {
                out.disjoint.push(es);
            }
        }
        K::NegativeObjectPropertyAssertion(..) | K::NegativeDataPropertyAssertion(..) => {
            return Err("negative property assertions");
        }
        K::HasKey(..) => return Err("keys"),
        K::FunctionalDataProperty(_) => return Err("functional data properties"),
        K::DataPropertyRange(..) => return Err("data property ranges"),
        K::DatatypeDefinition(..) => return Err("datatype definitions"),
        // annotations carry no logical content
        K::AnnotationAssertion(..)
        | K::SubAnnotationPropertyOf(..)
        | K::AnnotationPropertyDomain(..)
        | K::AnnotationPropertyRange(..) => {}
        // the profile check leaves the remaining types out of OWL 2 EL
        _ => {}
    }
    let data_builtin = match ax {
        K::SubDataPropertyOf(a, b) => is_builtin_data(a) || is_builtin_data(b),
        K::EquivalentDataProperties(ps) => ps.iter().any(is_builtin_data),
        K::DataPropertyDomain(d, _) | K::DataPropertyAssertion(d, ..) => is_builtin_data(d),
        _ => false,
    };
    if data_builtin {
        partial = Some(BUILTIN);
    }
    Ok(partial)
}

/// Whether a data property is the top or bottom one, which the classifier treats as an
/// ordinary property.
fn is_builtin_data(p: &DataProperty) -> bool {
    matches!(
        p.as_str(),
        vocab::OWL_TOP_DATA_PROPERTY | vocab::OWL_BOTTOM_DATA_PROPERTY
    )
}

fn is_top_or_bottom_role(p: &ObjectPropertyExpression) -> bool {
    matches!(
        p.named().as_str(),
        vocab::OWL_TOP_OBJECT_PROPERTY | vocab::OWL_BOTTOM_OBJECT_PROPERTY
    )
}

fn is_top_or_bottom_sub(s: &SubObjectPropertyExpression) -> bool {
    match s {
        SubObjectPropertyExpression::Expression(p) => is_top_or_bottom_role(p),
        SubObjectPropertyExpression::Chain(ps) => ps.iter().any(is_top_or_bottom_role),
    }
}

fn object_role(p: &ObjectPropertyExpression, it: &mut Interner) -> Option<Role> {
    match p {
        ObjectPropertyExpression::Property(p) => Some(it.role(RoleName::Object(p.clone()))),
        ObjectPropertyExpression::InverseOf(_) => None,
    }
}

/// The deepest nesting of class expressions that normalization follows, as the reverse
/// mapping bounds it.
const MAX_DEPTH: usize = 256;

/// The interned form of a class expression and, when the classifier may miss some of
/// its consequences, why. An expression the classifier cannot represent is an error
/// with the reason, and the axiom that holds it is left out: no stand-in for it is sound
/// on both sides of a subsumption.
pub fn class_expression(
    c: &ClassExpression,
    it: &mut Interner,
    depth: usize,
) -> Result<(Ex, Option<&'static str>), &'static str> {
    use ClassExpression as C;
    if depth > MAX_DEPTH {
        return Err("class expressions nested too deeply");
    }
    Ok(match c {
        C::Class(c) => (it.class(c), None),
        C::ObjectIntersectionOf(cs) => {
            let mut partial = None;
            let mut es: Vec<Ex> = cs
                .iter()
                .map(|c| {
                    let (e, p) = class_expression(c, it, depth + 1)?;
                    partial = partial.or(p);
                    Ok(e)
                })
                .collect::<Result<_, _>>()?;
            es.sort_unstable();
            es.dedup();
            es.retain(|&e| e != TOP);
            if es.contains(&BOTTOM) {
                return Ok((BOTTOM, partial));
            }
            let mut acc = match es.first() {
                None => return Ok((TOP, partial)),
                Some(&e) => e,
            };
            for &e in &es[1..] {
                acc = it.conj(acc, e);
            }
            (acc, partial)
        }
        C::ObjectSomeValuesFrom(p, f) => {
            let (f, partial) = class_expression(f, it, depth + 1)?;
            match p {
                ObjectPropertyExpression::Property(p)
                    if p.as_str() == vocab::OWL_BOTTOM_OBJECT_PROPERTY =>
                {
                    (BOTTOM, partial)
                }
                ObjectPropertyExpression::Property(p) => {
                    let r = it.role(RoleName::Object(p.clone()));
                    let partial = if p.as_str() == vocab::OWL_TOP_OBJECT_PROPERTY {
                        Some("the top and bottom object and data properties")
                    } else {
                        partial
                    };
                    if f == BOTTOM {
                        return Ok((BOTTOM, partial));
                    }
                    (it.node(Node::Exist(r, f)), partial)
                }
                ObjectPropertyExpression::InverseOf(_) => return Err("inverse properties"),
            }
        }
        C::ObjectHasValue(p, i) => {
            let f = match i {
                Individual::Named(i) => it.nominal(i),
                Individual::Anonymous(_) => return Err("anonymous individuals"),
            };
            match p {
                ObjectPropertyExpression::Property(p) => {
                    let r = it.role(RoleName::Object(p.clone()));
                    (it.node(Node::Exist(r, f)), Some("individuals"))
                }
                ObjectPropertyExpression::InverseOf(_) => return Err("inverse properties"),
            }
        }
        C::ObjectOneOf(is) if is.len() == 1 => match is.iter().next() {
            Some(Individual::Named(i)) => (it.nominal(i), Some("individuals")),
            _ => return Err("anonymous individuals"),
        },
        C::ObjectHasSelf(ObjectPropertyExpression::Property(p)) => {
            let r = it.role(RoleName::Object(p.clone()));
            (it.node(Node::SelfLoop(r)), Some("ObjectHasSelf"))
        }
        C::DataSomeValuesFrom(ps, r) if ps.len() == 1 => {
            let role = it.role(RoleName::Data(ps[0].clone()));
            let atom = data_range(r, it);
            let partial = match r {
                _ if is_builtin_data(&ps[0]) => {
                    Some("the top and bottom object and data properties")
                }
                DataRange::Datatype(d) if d.as_str() == vocab::RDFS_LITERAL => None,
                _ => Some("data ranges"),
            };
            (it.node(Node::Exist(role, atom)), partial)
        }
        C::DataHasValue(d, v) => {
            let role = it.role(RoleName::Data(d.clone()));
            let atom = it.data(DataAtom::Value(v.clone()));
            (it.node(Node::Exist(role, atom)), Some("data values"))
        }
        // outside OWL 2 EL: the profile check has already left the axiom out
        _ => return Err("a class expression outside OWL 2 EL"),
    })
}

fn data_range(r: &DataRange, it: &mut Interner) -> Ex {
    match r {
        DataRange::OneOf(ls) if ls.len() == 1 => {
            let l = ls
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| Literal::simple(""));
            it.data(DataAtom::Value(l))
        }
        r => it.data(DataAtom::Range(r.clone())),
    }
}

/// The rules each expression triggers.
#[derive(Default)]
pub struct Index {
    /// told superexpressions
    pub told: Vec<Vec<Ex>>,
    /// for an operand, its partner and the negative intersection they form
    pub conj: Vec<Vec<(Ex, Ex)>>,
    /// for a filler, the role and the negative existential restriction
    pub exist: Vec<Vec<(Role, Ex)>>,
    /// for a member, the disjointness sets it is in
    pub disjoint: Vec<Vec<u32>>,
    /// negative `ObjectHasSelf` restrictions: the role and the expression
    pub selfs: Vec<(Role, Ex)>,
    /// for an existential restriction that occurs positively, the context of its
    /// filler, which includes the role's range
    pub succ: FxHashMap<Ex, Ex>,
    pub roles: Roles,
}

impl Index {
    pub fn told(&self, e: Ex) -> &[Ex] {
        self.told.get(e as usize).map_or(&[], |v| v.as_slice())
    }
    pub fn conj(&self, e: Ex) -> &[(Ex, Ex)] {
        self.conj.get(e as usize).map_or(&[], |v| v.as_slice())
    }
    pub fn exist(&self, e: Ex) -> &[(Role, Ex)] {
        self.exist.get(e as usize).map_or(&[], |v| v.as_slice())
    }
    pub fn disjoint(&self, e: Ex) -> &[u32] {
        self.disjoint.get(e as usize).map_or(&[], |v| v.as_slice())
    }

    /// Builds the index of normalized axioms. It may intern new expressions: the
    /// intersections of fillers with ranges and the roles of chains.
    ///
    /// The property closure can grow with the square of the properties, so it stops with
    /// the bytes it reached when they pass `limit`, and when `stop` asks it to.
    pub fn build(
        ax: &Axioms,
        it: &mut Interner,
        limit: Option<usize>,
        stop: &dyn Fn() -> bool,
    ) -> Result<Index, Halt> {
        let mut ix = Index {
            roles: roles(ax, it, limit, stop)?,
            ..Index::default()
        };
        let mut neg_seen: FxHashSet<Ex> = FxHashSet::default();
        let mut pos_seen: FxHashSet<Ex> = FxHashSet::default();
        let mut told: Vec<(Ex, Ex)> = Vec::with_capacity(ax.subs.len());
        for &(a, b) in &ax.subs {
            negative(a, it, &mut ix, &mut neg_seen);
            positive(b, it, &mut ix, &mut pos_seen);
            if a != b {
                told.push((a, b));
            }
        }
        // a reflexive role links every individual to itself, so its range holds
        // everywhere
        let reflexive_ranges: Vec<Ex> = ix
            .roles
            .reflexive
            .iter()
            .filter_map(|&r| ix.roles.range[r as usize])
            .collect();
        for c in reflexive_ranges {
            positive(c, it, &mut ix, &mut pos_seen);
            told.push((TOP, c));
        }
        // a literal is in the range of its datatype, and every data range is in
        // rdfs:Literal
        let literal = it.data_ex(&DataAtom::Range(DataRange::Datatype(Datatype::literal())));
        for (e, n) in it.nodes.iter().enumerate() {
            let Node::Data(id) = *n else { continue };
            let e = e as Ex;
            if let Some(l) = literal
                && l != e
            {
                told.push((e, l));
            }
            if let DataAtom::Value(v) = &it.data[id as usize]
                && let Some(d) = it.data_ex(&DataAtom::Range(DataRange::Datatype(Datatype::new(
                    v.datatype(),
                ))))
            {
                told.push((e, d));
            }
        }
        let n = it.len();
        ix.told.resize_with(n, Vec::new);
        for (a, b) in told {
            ix.told[a as usize].push(b);
        }
        for v in &mut ix.told {
            v.sort_unstable();
            v.dedup();
        }
        ix.disjoint.resize_with(n, Vec::new);
        for (s, set) in ax.disjoint.iter().enumerate() {
            for &e in set {
                negative(e, it, &mut ix, &mut neg_seen);
                ix.disjoint[e as usize].push(s as u32);
            }
        }
        let n = it.len();
        ix.told.resize_with(n, Vec::new);
        ix.disjoint.resize_with(n, Vec::new);
        ix.conj.resize_with(n, Vec::new);
        ix.exist.resize_with(n, Vec::new);
        Ok(ix)
    }
}

fn grow<T: Default>(v: &mut Vec<T>, i: Ex) {
    if v.len() <= i as usize {
        v.resize_with(i as usize + 1, T::default);
    }
}

/// Registers a negative occurrence: the rules that build the expression from its parts.
fn negative(e: Ex, it: &mut Interner, ix: &mut Index, seen: &mut FxHashSet<Ex>) {
    if !seen.insert(e) {
        return;
    }
    match it.nodes[e as usize] {
        Node::Conj(a, b) => {
            grow(&mut ix.conj, a.max(b));
            ix.conj[a as usize].push((b, e));
            ix.conj[b as usize].push((a, e));
            negative(a, it, ix, seen);
            negative(b, it, ix, seen);
        }
        Node::Exist(r, f) => {
            grow(&mut ix.exist, f);
            ix.exist[f as usize].push((r, e));
            negative(f, it, ix, seen);
        }
        Node::SelfLoop(r) => ix.selfs.push((r, e)),
        _ => {}
    }
}

/// Registers a positive occurrence: the successor context of each existential
/// restriction in it.
fn positive(e: Ex, it: &mut Interner, ix: &mut Index, seen: &mut FxHashSet<Ex>) {
    if !seen.insert(e) {
        return;
    }
    match it.nodes[e as usize] {
        Node::Conj(a, b) => {
            positive(a, it, ix, seen);
            positive(b, it, ix, seen);
        }
        Node::Exist(r, f) => {
            positive(f, it, ix, seen);
            let succ = match ix.roles.range.get(r as usize).copied().flatten() {
                Some(range) => {
                    positive(range, it, ix, seen);
                    it.conj(f, range)
                }
                None => f,
            };
            ix.succ.insert(e, succ);
        }
        _ => {}
    }
}

/// The role table: the reflexive and transitive closure of the role hierarchy, the
/// binarized chains with their compositions, the reflexive roles and the ranges.
/// Why building the index stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Halt {
    /// the property closure passed the memory limit, with the bytes it reached
    Memory(usize),
    /// the run's deadline or cancellation came
    Stopped,
}

/// The steps of the property closure between two polls of the run's deadline and
/// cancellation, where a step visits one edge or one entry.
const POLL_WORK: usize = 65_536;

fn roles(
    ax: &Axioms,
    it: &mut Interner,
    limit: Option<usize>,
    stop: &dyn Fn() -> bool,
) -> Result<Roles, Halt> {
    let bytes = Cell::new(0usize);
    let grow = |b: usize| -> Result<(), Halt> {
        bytes.set(bytes.get() + b);
        if limit.is_some_and(|l| bytes.get() > l) {
            return Err(Halt::Memory(bytes.get()));
        }
        Ok(())
    };
    let work = Cell::new(0usize);
    let tick = |w: usize| -> Result<(), Halt> {
        let before = work.get();
        work.set(before + w);
        if before / POLL_WORK != (before + w) / POLL_WORK && stop() {
            return Err(Halt::Stopped);
        }
        Ok(())
    };
    // binarize chains: r1 ∘ r2 ∘ … ∘ rn ⊑ s becomes r1 ∘ r2 ⊑ u1, u1 ∘ r3 ⊑ u2, …
    let mut binary: Vec<(Role, Role, Role)> = Vec::new();
    let mut prefixes: FxHashMap<(Role, Role), Role> = FxHashMap::default();
    for (rs, s) in &ax.chains {
        let mut left = rs[0];
        for (k, &r) in rs.iter().enumerate().skip(1) {
            if k + 1 == rs.len() {
                binary.push((left, r, *s));
            } else {
                left = match prefixes.get(&(left, r)) {
                    Some(&u) => u,
                    None => {
                        let u = it.fresh_role();
                        prefixes.insert((left, r), u);
                        binary.push((left, r, u));
                        u
                    }
                };
            }
        }
    }
    let n = it.roles.len();
    let mut direct: Vec<Vec<Role>> = vec![Vec::new(); n];
    for &(r, s) in &ax.role_subs {
        direct[r as usize].push(s);
    }
    // every object property is a subproperty of the top object property
    if let Some(&top) = it.role_ids.get(&RoleName::Object(ObjectProperty::top())) {
        for (r, name) in it.roles.iter().enumerate() {
            if matches!(name, RoleName::Object(_)) && r as Role != top {
                direct[r].push(top);
            }
        }
    }
    let mut sup: Vec<Vec<Role>> = Vec::with_capacity(n);
    for r in 0..n as Role {
        let mut seen: BTreeSet<Role> = BTreeSet::new();
        let mut stack = vec![r];
        while let Some(x) = stack.pop() {
            tick(1)?;
            if seen.insert(x) {
                stack.extend(direct[x as usize].iter().copied());
            }
        }
        // the closure and its inverse, which the chains read
        grow(48 + seen.len() * 8)?;
        sup.push(seen.into_iter().collect());
    }
    let mut sub: Vec<Vec<Role>> = vec![Vec::new(); n];
    for (r, ss) in sup.iter().enumerate() {
        tick(1 + ss.len())?;
        for &s in ss {
            sub[s as usize].push(r as Role);
        }
    }
    let mut comp: FxHashMap<(Role, Role), Vec<Role>> = FxHashMap::default();
    let mut left = vec![false; n];
    let mut right = vec![false; n];
    for &(s1, s2, t) in &binary {
        for &r1 in &sub[s1 as usize] {
            left[r1 as usize] = true;
            let mut b = 0;
            for &r2 in &sub[s2 as usize] {
                right[r2 as usize] = true;
                let v = comp.entry((r1, r2)).or_insert_with(|| {
                    b += 40;
                    Vec::new()
                });
                v.push(t);
                b += 4;
            }
            grow(b)?;
            tick(1 + sub[s2 as usize].len())?;
        }
    }
    for v in comp.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    let mut reflexive: BTreeSet<Role> = BTreeSet::new();
    for &r in &ax.reflexive {
        tick(1 + sup[r as usize].len())?;
        reflexive.extend(sup[r as usize].iter().copied());
    }
    let mut direct_ranges: Vec<Vec<Ex>> = vec![Vec::new(); n];
    for &(r, c) in &ax.ranges {
        direct_ranges[r as usize].push(c);
    }
    let mut range = vec![None; n];
    for r in 0..n {
        tick(1 + sup[r].len())?;
        let mut cs: Vec<Ex> = sup[r]
            .iter()
            .flat_map(|&s| direct_ranges[s as usize].iter().copied())
            .collect();
        cs.sort_unstable();
        cs.dedup();
        cs.retain(|&c| c != TOP);
        if let Some(&first) = cs.first() {
            let mut acc = first;
            for &c in &cs[1..] {
                acc = it.conj(acc, c);
            }
            range[r] = Some(acc);
        }
    }
    Ok(Roles {
        sup,
        comp,
        right,
        left,
        reflexive: reflexive.into_iter().collect(),
        range,
    })
}
