//! OWL 2 QL query rewriting (spec F17 §7): a basic graph pattern becomes a union of basic
//! graph patterns over the stored facts, by the PerfectRef procedure of Calvanese, De
//! Giacomo, Lembo, Lenzerini and Rosati ("Tractable reasoning and efficient query answering
//! in description logics: the DL-Lite family", 2007).
//!
//! **The TBox.** [`Tbox::read`] reads the terminology of the input graphs with
//! `sparkles-owl` and keeps the axioms that the OWL 2 QL grammar allows
//! ([`sparkles_owl::profiles::partition`]). It keeps their positive inclusions in DL-Lite_R
//! form: `B1 ⊑ B2` between basic concepts, a class `A` or `∃R` for a property or inverse
//! property `R`, and `R1 ⊑ R2` between properties. A qualified existential `B ⊑ ∃R.A` is
//! normalized with a fresh property `Q`, as `Q ⊑ R`, `B ⊑ ∃Q` and `∃Q⁻ ⊑ A`. No stored fact
//! uses `Q`, so a branch that still names it matches nothing and is dropped. Negative
//! inclusions (disjointness, irreflexivity, asymmetry and complements) do not change the
//! answers of a consistent ontology and are left out. Reflexive properties and inclusions
//! into or out of `owl:Thing` are not used, and the TBox lists them with the axioms outside
//! the profile.
//!
//! **PerfectRef.** An atom is `A(t)` for a pattern `t rdf:type A` with a class `A`, or
//! `P(t1, t2)` for a pattern `t1 P t2`. A term is a variable, a constant or `_`, a
//! non-distinguished variable that occurs once. A variable is distinguished when the query
//! uses it outside the pattern: in a projection, a filter, another pattern, a CONSTRUCT
//! template or anything else, and every variable is when the query counts its solutions
//! with `COUNT(*)`. The procedure replaces each atom by the atoms that imply it under an
//! applicable inclusion, unifies pairs of atoms, and repeats until no new query appears.
//! `A(x)` is implied through `B ⊑ A`, `P(x, _)` through `B ⊑ ∃P`, `P(_, x)` through
//! `B ⊑ ∃P⁻` and `P(x, y)` through `R ⊑ P`. A distinguished variable that a unification
//! binds to a constant or to another distinguished variable is bound again in the branch,
//! with `BIND`, so that every branch gives the same variables.
//!
//! **The result.** A rewritten pattern is `DISTINCT` over the projection of its
//! distinguished variables of the union of its branches: an answer is a set of bindings of
//! those variables, as the certain answers of an ontology are, so a solution counts once
//! however many values its existential variables have. A pattern that no inclusion changes
//! is left as it is. A pattern that the procedure cannot express, such as one with a
//! variable predicate or class, a predicate or class of the RDF, RDFS or OWL vocabularies,
//! or an RDF-star term, is left for the `owl-rl` demand program, and so is a pattern whose
//! rewriting passes the bound on branches. The union streams as the planner plans any
//! union.

use crate::inputs::{self, Inputs};
use oxrdf::{NamedNode, Term, Variable};
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use spargebra::Query;
use spargebra::algebra::{AggregateExpression, Expression, GraphPattern, OrderExpression};
use spargebra::term::{NamedNodePattern, TermPattern, TriplePattern};
use sparkles_core::error::Result;
use sparkles_core::id::Id;
use sparkles_core::index::Perm;
use sparkles_core::sparql::exec::PlanInfo;
use sparkles_core::sparql::reasoning::{Rewritten, SessionRequest};
use sparkles_core::store::Snapshot;
use sparkles_owl::model::{
    AxiomKind, ClassExpression as Ce, DataRange, ObjectPropertyExpression as Ope,
    SubObjectPropertyExpression,
};
use std::sync::Arc;

/// The most branches the rewriting of one basic graph pattern may have, by default
/// (`budget.maxUnion`, spec F17 §7).
pub const DEFAULT_MAX_UNION: usize = 1_024;

/// An estimate of the bytes one terminology triple takes while the axioms are parsed: the
/// triple, its parsed structure and its share of the ontology's indexes.
const TRIPLE_BYTES: usize = 512;

/// The most axioms outside the profile that a TBox names in its warnings.
const MAX_LISTED: usize = 20;

const OWL: &str = "http://www.w3.org/2002/07/owl#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const RDFS_LITERAL: &str = "http://www.w3.org/2000/01/rdf-schema#Literal";

/// A property or the inverse of one, by symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Role {
    p: u32,
    inv: bool,
}

impl Role {
    fn inverse(self) -> Role {
        Role {
            p: self.p,
            inv: !self.inv,
        }
    }
}

/// A basic concept of DL-Lite: a class, or the domain of a role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Basic {
    Class(u32),
    Exists(Role),
}

/// The positive inclusions of an OWL 2 QL ontology in DL-Lite_R form (see the module
/// documentation).
#[derive(Debug, Default)]
pub struct Tbox {
    /// the IRI of each symbol; `None` for a property made by normalization
    names: Vec<Option<String>>,
    index: FxHashMap<String, u32>,
    /// for a basic concept, the basic concepts it directly subsumes (`B ⊑ key`)
    concepts: FxHashMap<Basic, Vec<Basic>>,
    /// for a named property `p`, the roles `R` with `R ⊑ p`
    roles: FxHashMap<u32, Vec<Role>>,
    /// the axioms of the terminology the profile allows
    pub axioms: usize,
    /// the axioms outside OWL 2 QL, as messages, at most [`MAX_LISTED`]
    pub outside: Vec<String>,
    /// the number of axioms outside OWL 2 QL
    pub outside_count: usize,
    /// the axioms of OWL 2 QL that the rewriting does not use, as messages
    pub unused: Vec<String>,
    /// the terminology triples that map to no OWL axiom, such as the subproperty triple
    /// of an undeclared property
    pub unmapped: usize,
}

impl Tbox {
    fn sym(&mut self, iri: &str) -> u32 {
        if let Some(&s) = self.index.get(iri) {
            return s;
        }
        let s = self.names.len() as u32;
        self.names.push(Some(iri.to_string()));
        self.index.insert(iri.to_string(), s);
        s
    }

    fn fresh(&mut self) -> u32 {
        self.names.push(None);
        (self.names.len() - 1) as u32
    }

    fn lookup(&self, iri: &str) -> Option<u32> {
        self.index.get(iri).copied()
    }

    fn is_aux(&self, s: u32) -> bool {
        self.names.get(s as usize).is_some_and(|n| n.is_none())
    }

    /// Whether any inclusion has `p` on its right: a pattern on `p` can change.
    fn affects_role(&self, p: u32) -> bool {
        self.roles.get(&p).is_some_and(|v| !v.is_empty())
            || [false, true].into_iter().any(|inv| {
                self.concepts
                    .get(&Basic::Exists(Role { p, inv }))
                    .is_some_and(|v| !v.is_empty())
            })
    }

    fn affects_class(&self, a: u32) -> bool {
        self.concepts
            .get(&Basic::Class(a))
            .is_some_and(|v| !v.is_empty())
    }

    fn add_concept(&mut self, sub: Basic, sup: Basic) {
        if sub != sup {
            let v = self.concepts.entry(sup).or_default();
            if !v.contains(&sub) {
                v.push(sub);
            }
        }
    }

    /// `sub ⊑ sup` between roles.
    fn add_role(&mut self, sub: Role, sup: Role) {
        let (sub, sup) = if sup.inv {
            (sub.inverse(), sup.inverse())
        } else {
            (sub, sup)
        };
        if sub != sup {
            let v = self.roles.entry(sup.p).or_default();
            if !v.contains(&sub) {
                v.push(sub);
            }
        }
    }

    fn role(&mut self, r: &Ope) -> Role {
        match r {
            Ope::Property(p) => Role {
                p: self.sym(p.as_str()),
                inv: false,
            },
            Ope::InverseOf(p) => Role {
                p: self.sym(p.as_str()),
                inv: true,
            },
        }
    }

    /// The basic concept of a subclass expression, if OWL 2 QL allows it there and the
    /// rewriting uses it.
    fn sub_basic(&mut self, c: &Ce) -> Option<Basic> {
        match c {
            Ce::Class(a) if a.as_str() == OWL_THING => None,
            Ce::Class(a) => Some(Basic::Class(self.sym(a.as_str()))),
            Ce::ObjectSomeValuesFrom(r, f) if matches!(&**f, Ce::Class(t) if t.as_str() == OWL_THING) => {
                Some(Basic::Exists(self.role(r)))
            }
            Ce::DataSomeValuesFrom(us, d)
                if us.len() == 1
                    && matches!(d, DataRange::Datatype(t) if t.as_str() == RDFS_LITERAL) =>
            {
                Some(Basic::Exists(Role {
                    p: self.sym(us[0].as_str()),
                    inv: false,
                }))
            }
            _ => None,
        }
    }

    /// `sub ⊑ sup` for a superclass expression `sup`. Returns whether it was used.
    fn add_super(&mut self, sub: Basic, sup: &Ce) -> bool {
        match sup {
            Ce::Class(a) if a.as_str() == OWL_THING => true,
            Ce::Class(a) => {
                let a = self.sym(a.as_str());
                self.add_concept(sub, Basic::Class(a));
                true
            }
            Ce::ObjectIntersectionOf(cs) => {
                let mut used = true;
                for c in cs {
                    used &= self.add_super(sub, c);
                }
                used
            }
            // a negative inclusion: no answer of a consistent ontology changes
            Ce::ObjectComplementOf(_) => true,
            Ce::ObjectSomeValuesFrom(r, f) => {
                let r = self.role(r);
                match &**f {
                    Ce::Class(t) if t.as_str() == OWL_THING => {
                        self.add_concept(sub, Basic::Exists(r));
                        true
                    }
                    Ce::Class(a) => {
                        // B ⊑ ∃R.A as Q ⊑ R, B ⊑ ∃Q and ∃Q⁻ ⊑ A
                        let a = self.sym(a.as_str());
                        let q = Role {
                            p: self.fresh(),
                            inv: false,
                        };
                        self.add_role(q, r);
                        self.add_concept(sub, Basic::Exists(q));
                        self.add_concept(Basic::Exists(q.inverse()), Basic::Class(a));
                        true
                    }
                    _ => false,
                }
            }
            // the data range of a data property's values does not bind a variable
            Ce::DataSomeValuesFrom(us, _) if us.len() == 1 => {
                let u = Role {
                    p: self.sym(us[0].as_str()),
                    inv: false,
                };
                self.add_concept(sub, Basic::Exists(u));
                true
            }
            _ => false,
        }
    }

    fn add_axiom(&mut self, ax: &AxiomKind) {
        use AxiomKind as K;
        let used = match ax {
            K::SubClassOf(sub, sup) => match self.sub_basic(sub) {
                Some(b) => self.add_super(b, sup),
                None => false,
            },
            K::EquivalentClasses(cs) => {
                let cs: Vec<&Ce> = cs.iter().collect();
                let mut used = true;
                for (i, a) in cs.iter().enumerate() {
                    for (j, b) in cs.iter().enumerate() {
                        if i != j {
                            used &= match self.sub_basic(a) {
                                Some(x) => self.add_super(x, b),
                                None => false,
                            };
                        }
                    }
                }
                used
            }
            K::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(a), b) => {
                let (a, b) = (self.role(a), self.role(b));
                self.add_role(a, b);
                true
            }
            K::EquivalentObjectProperties(rs) => {
                let rs: Vec<Role> = rs.iter().map(|r| self.role(r)).collect();
                for &a in &rs {
                    for &b in &rs {
                        self.add_role(a, b);
                    }
                }
                true
            }
            K::InverseObjectProperties(a, b) => {
                let (a, b) = (self.role(a), self.role(b));
                self.add_role(a, b.inverse());
                self.add_role(b, a.inverse());
                true
            }
            K::SymmetricObjectProperty(r) => {
                let r = self.role(r);
                self.add_role(r, r.inverse());
                true
            }
            K::ObjectPropertyDomain(r, c) => {
                let r = self.role(r);
                self.add_super(Basic::Exists(r), c)
            }
            K::ObjectPropertyRange(r, c) => {
                let r = self.role(r);
                self.add_super(Basic::Exists(r.inverse()), c)
            }
            K::SubDataPropertyOf(a, b) => {
                let (a, b) = (self.sym(a.as_str()), self.sym(b.as_str()));
                self.add_role(Role { p: a, inv: false }, Role { p: b, inv: false });
                true
            }
            K::EquivalentDataProperties(us) => {
                let us: Vec<u32> = us.iter().map(|u| self.sym(u.as_str())).collect();
                for &a in &us {
                    for &b in &us {
                        self.add_role(Role { p: a, inv: false }, Role { p: b, inv: false });
                    }
                }
                true
            }
            K::DataPropertyDomain(u, c) => {
                let u = Role {
                    p: self.sym(u.as_str()),
                    inv: false,
                };
                self.add_super(Basic::Exists(u), c)
            }
            // negative inclusions, datatypes of values, declarations and annotations change
            // no answer of a consistent ontology; assertions are data, which the rewritten
            // query reads where it is stored
            K::DisjointClasses(_)
            | K::DisjointObjectProperties(_)
            | K::DisjointDataProperties(_)
            | K::IrreflexiveObjectProperty(_)
            | K::AsymmetricObjectProperty(_)
            | K::DataPropertyRange(..)
            | K::DatatypeDefinition(..)
            | K::Declaration(_)
            | K::DifferentIndividuals(_)
            | K::ClassAssertion(..)
            | K::ObjectPropertyAssertion(..)
            | K::DataPropertyAssertion(..)
            | K::NegativeObjectPropertyAssertion(..)
            | K::NegativeDataPropertyAssertion(..)
            | K::AnnotationAssertion(..)
            | K::SubAnnotationPropertyOf(..)
            | K::AnnotationPropertyDomain(..)
            | K::AnnotationPropertyRange(..) => true,
            _ => false,
        };
        if !used && self.unused.len() < MAX_LISTED {
            self.unused.push(format!("{ax:?}"));
        }
    }

    /// The TBox of the OWL 2 QL axioms among `triples`, the terminology of an ontology.
    pub fn from_triples(triples: Vec<oxrdf::Triple>) -> Tbox {
        let ontology = sparkles_owl::read::read_graph(
            triples,
            sparkles_owl::ReadOptions {
                terminology_only: true,
                ..Default::default()
            },
        );
        let part =
            sparkles_owl::profiles::partition(&ontology, sparkles_owl::profiles::Profile::Ql);
        let mut t = Tbox {
            unmapped: ontology.unmapped.len(),
            ..Default::default()
        };
        let outside = part.outside_axioms();
        t.outside_count = outside.len();
        for v in part.outside.iter().take(MAX_LISTED) {
            t.outside.push(v.message.clone());
        }
        for ax in &part.inside {
            t.axioms += 1;
            t.add_axiom(&ax.kind);
        }
        t
    }

    /// The TBox of the terminology in `graphs` of `snap`: the triples of the structural
    /// predicates of OWL and RDFS and the `rdf:type` triples of their structural types.
    /// The triples read, about [`TRIPLE_BYTES`] each while the axioms are parsed, are
    /// charged to `budget`.
    pub(crate) fn read(
        snap: &Snapshot,
        graphs: &[Id],
        stop: &dyn Fn() -> Result<()>,
        budget: &crate::budget::Budget,
    ) -> Result<Tbox> {
        let mut triples = Vec::new();
        let mut keep = |keys: Vec<sparkles_core::index::Key>| -> Result<()> {
            for (i, k) in keys.iter().enumerate() {
                if i % 4096 == 0 {
                    stop()?;
                }
                if i % 1024 == 0 {
                    budget
                        .try_charge(1024 * TRIPLE_BYTES)
                        .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL axioms"))?;
                }
                let q = Perm::Pos.to_quad(k);
                if !graphs.contains(&q[3]) {
                    continue;
                }
                let (Some(s), Some(Term::NamedNode(p)), Some(o)) =
                    (snap.term(q[0]), snap.term(q[1]), snap.term(q[2]))
                else {
                    continue;
                };
                let s = match s {
                    Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                    Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                    _ => continue,
                };
                triples.push(oxrdf::Triple::new(s, p, o));
            }
            Ok(())
        };
        for p in sparkles_owl::vocab::STRUCTURAL_PREDICATES {
            if let Some(id) = snap.lookup_iri(p) {
                keep(snap.scan_keys(Perm::Pos, &[id.0])?)?;
            }
        }
        if let Some(ty) = snap.lookup_iri(oxrdf::vocab::rdf::TYPE.as_str()) {
            for c in sparkles_owl::vocab::STRUCTURAL_TYPES {
                if let Some(id) = snap.lookup_iri(c) {
                    keep(snap.scan_keys(Perm::Pos, &[ty.0, id.0])?)?;
                }
            }
        }
        stop()?;
        Ok(Tbox::from_triples(triples))
    }

    /// An estimate of the bytes the TBox holds.
    fn approx_bytes(&self) -> usize {
        let names: usize = self
            .names
            .iter()
            .map(|n| 2 * n.as_ref().map_or(0, |s| s.len()) + 64)
            .sum();
        let lists: usize = self
            .concepts
            .values()
            .map(|v| v.len() * 12 + 48)
            .sum::<usize>()
            + self.roles.values().map(|v| v.len() * 8 + 40).sum::<usize>();
        names
            + lists
            + self
                .outside
                .iter()
                .chain(&self.unused)
                .map(|m| m.len())
                .sum::<usize>()
    }

    /// The warnings a query under the TBox carries: the axioms it does not use.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.outside_count > 0 {
            out.push(format!(
                "{} axioms of the ontology are outside OWL 2 QL and are ignored by the \
                 rewriting{}",
                self.outside_count,
                match self.outside.first() {
                    Some(m) => format!(", such as: {m}"),
                    None => String::new(),
                }
            ));
        }
        if self.unmapped > 0 {
            out.push(format!(
                "{} triples of the terminology are not OWL axioms and are ignored by the \
                 rewriting, such as a triple about an undeclared property",
                self.unmapped
            ));
        }
        if !self.unused.is_empty() {
            out.push(format!(
                "{} axioms of OWL 2 QL are not used by the rewriting, such as: {}",
                self.unused.len(),
                self.unused[0]
            ));
        }
        out
    }
}

/// The TBoxes of recent snapshots and views, for queries on the same commit.
static TBOXES: Mutex<Vec<(String, Arc<Tbox>)>> = Mutex::new(Vec::new());
const KEPT_TBOXES: usize = 8;

/// Drop the kept TBoxes.
pub fn clear() {
    TBOXES.lock().clear();
}

/// The TBox of the input graphs of `inputs` that the caller of `req` reads, from the
/// TBoxes kept for the same generation, commit and view, or read from the snapshot.
pub(crate) fn tbox(req: &SessionRequest, inputs: &Inputs) -> Result<Arc<Tbox>> {
    let snap = &req.snap;
    let resolved = match &req.readable {
        Some(readable) => inputs::resolve_readable(snap, inputs, &|g| readable(g)),
        None => inputs::resolve(snap, inputs),
    };
    let mut graphs = resolved.ids(snap);
    if let Some(readable) = &req.readable {
        graphs.retain(|g| readable(*g));
    }
    let key = (snap.cacheable && !req.no_cache).then(|| {
        format!(
            "{}|{}|{}|{:?}",
            snap.generation.uid,
            snap.commit,
            snap.mask.as_ref().map_or("", |m| m.key.as_str()),
            graphs
        )
    });
    if let Some(k) = &key {
        let mut kept = TBOXES.lock();
        if let Some(i) = kept.iter().position(|(x, _)| x == k) {
            let e = kept.remove(i);
            let t = e.1.clone();
            kept.push(e);
            return Ok(t);
        }
    }
    // the TBox and the triples it is read from live while the query plans, so the
    // query's memory budget bounds them
    let budget =
        crate::budget::Budget::new(req.max_memory.map(|m| m.min(usize::MAX as u64) as usize));
    let t = Arc::new(Tbox::read(snap, &graphs, &*req.stop, &budget)?);
    budget
        .try_charge(t.approx_bytes())
        .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL axioms"))?;
    if let Some(k) = key {
        let mut kept = TBOXES.lock();
        kept.push((k, t.clone()));
        if kept.len() > KEPT_TBOXES {
            kept.remove(0);
        }
    }
    Ok(t)
}

// ------------------------------------------------------------------ the queries ------

/// A term of an atom.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum T {
    Var(u32),
    Const(u32),
    /// a non-distinguished variable that occurs once
    Free,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Atom {
    C(u32, T),
    R(u32, T, T),
}

impl Atom {
    fn terms(&self) -> impl Iterator<Item = T> {
        let (a, b) = match *self {
            Atom::C(_, t) => (t, None),
            Atom::R(_, s, o) => (s, Some(o)),
        };
        std::iter::once(a).chain(b)
    }

    fn map(&self, f: impl Fn(T) -> T) -> Atom {
        match *self {
            Atom::C(a, t) => Atom::C(a, f(t)),
            Atom::R(p, s, o) => Atom::R(p, f(s), f(o)),
        }
    }
}

/// A conjunctive query of the rewriting: its atoms, and the distinguished variables that
/// unification bound, each to a constant or to another distinguished variable.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Cq {
    atoms: Vec<Atom>,
    binds: Vec<(u32, T)>,
}

/// The variables and constants of one basic graph pattern.
struct Bgp<'a> {
    tbox: &'a Tbox,
    /// each variable as the query wrote it, and whether it is distinguished
    vars: Vec<(TermPattern, bool)>,
    consts: Vec<Term>,
}

impl Bgp<'_> {
    fn distinguished(&self, t: T) -> bool {
        matches!(t, T::Var(v) if self.vars[v as usize].1)
    }

    /// `_` for a non-distinguished variable that occurs once, atoms sorted and each once.
    fn normalize(&self, mut q: Cq) -> Cq {
        let mut count: FxHashMap<u32, u32> = FxHashMap::default();
        for a in &q.atoms {
            for t in a.terms() {
                if let T::Var(v) = t {
                    *count.entry(v).or_default() += 1;
                }
            }
        }
        for a in &mut q.atoms {
            *a = a.map(|t| match t {
                T::Var(v) if !self.vars[v as usize].1 && count.get(&v) == Some(&1) => T::Free,
                t => t,
            });
        }
        q.atoms.sort_unstable();
        q.atoms.dedup();
        q.binds.sort_unstable();
        q.binds.dedup();
        q
    }

    /// The atoms that imply `a` under one inclusion (`gr` of PerfectRef).
    fn implied(&self, a: Atom, out: &mut Vec<Atom>) {
        let t = self.tbox;
        let from_basic = |b: Basic, x: T, out: &mut Vec<Atom>| match b {
            Basic::Class(c) => out.push(Atom::C(c, x)),
            Basic::Exists(Role { p, inv: false }) => out.push(Atom::R(p, x, T::Free)),
            Basic::Exists(Role { p, inv: true }) => out.push(Atom::R(p, T::Free, x)),
        };
        match a {
            Atom::C(c, x) => {
                for &b in t.concepts.get(&Basic::Class(c)).into_iter().flatten() {
                    from_basic(b, x, out);
                }
            }
            Atom::R(p, s, o) => {
                for &r in t.roles.get(&p).into_iter().flatten() {
                    out.push(if r.inv {
                        Atom::R(r.p, o, s)
                    } else {
                        Atom::R(r.p, s, o)
                    });
                }
                if o == T::Free {
                    let e = Basic::Exists(Role { p, inv: false });
                    for &b in t.concepts.get(&e).into_iter().flatten() {
                        from_basic(b, s, out);
                    }
                }
                if s == T::Free {
                    let e = Basic::Exists(Role { p, inv: true });
                    for &b in t.concepts.get(&e).into_iter().flatten() {
                        from_basic(b, o, out);
                    }
                }
            }
        }
    }

    /// The query with atoms `i` and `j` unified by their most general unifier, if they
    /// unify (`reduce` of PerfectRef).
    fn reduce(&self, q: &Cq, i: usize, j: usize) -> Option<Cq> {
        let (a, b) = (q.atoms[i], q.atoms[j]);
        let pairs: Vec<(T, T)> = match (a, b) {
            (Atom::C(x, s), Atom::C(y, t)) if x == y => vec![(s, t)],
            (Atom::R(x, s1, o1), Atom::R(y, s2, o2)) if x == y => vec![(s1, s2), (o1, o2)],
            _ => return None,
        };
        let mut sub: FxHashMap<u32, T> = FxHashMap::default();
        let mut binds = q.binds.clone();
        let find = |sub: &FxHashMap<u32, T>, mut t: T| {
            while let T::Var(v) = t {
                match sub.get(&v) {
                    Some(&n) => t = n,
                    None => break,
                }
            }
            t
        };
        for (x, y) in pairs {
            let (x, y) = (find(&sub, x), find(&sub, y));
            match (x, y) {
                _ if x == y => {}
                (T::Free, _) | (_, T::Free) => {}
                (T::Const(_), T::Const(_)) => return None,
                (T::Var(v), c @ T::Const(_)) | (c @ T::Const(_), T::Var(v)) => {
                    sub.insert(v, c);
                    if self.vars[v as usize].1 {
                        binds.push((v, c));
                    }
                }
                (T::Var(v), T::Var(w)) => {
                    // bind a non-distinguished variable rather than a distinguished one
                    let (from, to) = if !self.vars[w as usize].1 {
                        (w, v)
                    } else {
                        (v, w)
                    };
                    sub.insert(from, T::Var(to));
                    if self.vars[from as usize].1 {
                        binds.push((from, T::Var(to)));
                    }
                }
            }
        }
        // the unified atom: a `_` gives way to the other side's term
        let merge = |x: T, y: T| {
            let (x, y) = (find(&sub, x), find(&sub, y));
            if x == T::Free { y } else { x }
        };
        let unified = match (a, b) {
            (Atom::C(c, s), Atom::C(_, t)) => Atom::C(c, merge(s, t)),
            (Atom::R(p, s1, o1), Atom::R(_, s2, o2)) => Atom::R(p, merge(s1, s2), merge(o1, o2)),
            _ => unreachable!("atoms of different kinds do not unify"),
        };
        let mut atoms: Vec<Atom> = Vec::with_capacity(q.atoms.len() - 1);
        for (k, at) in q.atoms.iter().enumerate() {
            if k == i {
                atoms.push(unified);
            } else if k != j {
                atoms.push(at.map(|t| find(&sub, t)));
            }
        }
        let binds = binds.into_iter().map(|(v, t)| (v, find(&sub, t))).collect();
        Some(self.normalize(Cq { atoms, binds }))
    }

    /// PerfectRef from `q0`, or `None` when the rewriting passes `max` queries.
    fn perfect_ref(
        &self,
        q0: Cq,
        max: usize,
        stop: &dyn Fn() -> Result<()>,
    ) -> Result<Option<Vec<Cq>>> {
        let q0 = self.normalize(q0);
        let mut seen: FxHashSet<Cq> = FxHashSet::default();
        let mut order: Vec<Cq> = Vec::new();
        seen.insert(q0.clone());
        order.push(q0);
        let mut next = 0;
        let mut implied = Vec::new();
        while next < order.len() {
            if next % 64 == 0 {
                stop()?;
            }
            let q = order[next].clone();
            next += 1;
            let mut found: Vec<Cq> = Vec::new();
            for i in 0..q.atoms.len() {
                implied.clear();
                self.implied(q.atoms[i], &mut implied);
                for &a in &implied {
                    let mut atoms = q.atoms.clone();
                    atoms[i] = a;
                    found.push(self.normalize(Cq {
                        atoms,
                        binds: q.binds.clone(),
                    }));
                }
            }
            for i in 0..q.atoms.len() {
                for j in i + 1..q.atoms.len() {
                    if let Some(r) = self.reduce(&q, i, j) {
                        found.push(r);
                    }
                }
            }
            for f in found {
                if seen.insert(f.clone()) {
                    order.push(f);
                    if order.len() > max {
                        return Ok(None);
                    }
                }
            }
        }
        // a branch that names a property of the normalization matches no stored fact, and
        // a branch whose atoms hold another's, with the same bindings, adds no answer
        order.retain(|q| {
            !q.atoms.iter().any(|a| match a {
                Atom::C(..) => false,
                Atom::R(p, ..) => self.tbox.is_aux(*p),
            })
        });
        let sets: Vec<FxHashSet<Atom>> = order
            .iter()
            .map(|q| q.atoms.iter().copied().collect())
            .collect();
        let keep: Vec<bool> = (0..order.len())
            .map(|i| {
                !(0..order.len()).any(|j| {
                    j != i
                        && order[j].binds == order[i].binds
                        && sets[j].len() < sets[i].len()
                        && sets[j].is_subset(&sets[i])
                })
            })
            .collect();
        let mut out: Vec<Cq> = Vec::new();
        for (q, k) in order.into_iter().zip(keep) {
            if k {
                out.push(q);
            }
        }
        Ok(Some(out))
    }
}

/// The count of each variable and blank node in the SSE form of `text`, by name with its
/// sigil. A name inside a string literal counts too, which errs towards distinguished.
fn count_names(text: &str, out: &mut FxHashMap<String, i64>, by: i64) {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let sigil = if b[i] == b'?' {
            1
        } else if b[i] == b'_' && b.get(i + 1) == Some(&b':') {
            2
        } else {
            i += 1;
            continue;
        };
        i += sigil;
        while i < b.len() && !b[i].is_ascii_whitespace() && !b"()[]{}\"'".contains(&b[i]) {
            i += 1;
        }
        if i > start + sigil {
            *out.entry(text[start..i].to_string()).or_default() += by;
        }
    }
}

fn name_of(t: &TermPattern) -> Option<String> {
    match t {
        TermPattern::Variable(v) => Some(format!("?{}", v.as_str())),
        TermPattern::BlankNode(b) => Some(format!("_:{}", b.as_str())),
        _ => None,
    }
}

fn reserved(iri: &str) -> bool {
    [OWL, RDF, RDFS, XSD].iter().any(|ns| iri.starts_with(ns))
}

/// What became of one basic graph pattern.
enum Outcome {
    /// no inclusion changes it
    Unchanged,
    /// the union of its branches
    Rewritten(GraphPattern),
    /// left for derivation, for the reason given
    Derive(String),
}

struct Rewriter<'a> {
    tbox: &'a Tbox,
    max_union: usize,
    stop: &'a dyn Fn() -> Result<()>,
    /// the occurrences of each name in the whole query
    counts: FxHashMap<String, i64>,
    /// every variable is distinguished (`COUNT(*)`)
    all_distinguished: bool,
    fresh: usize,
    derive: Vec<Vec<TriplePattern>>,
    nodes: Vec<PlanInfo>,
    warnings: Vec<String>,
}

impl Rewriter<'_> {
    fn fresh_var(&mut self) -> Variable {
        self.fresh += 1;
        Variable::new_unchecked(format!("__ql{}", self.fresh))
    }

    fn bgp(&mut self, patterns: &[TriplePattern]) -> Result<Outcome> {
        let t0 = std::time::Instant::now();
        let tbox = self.tbox;
        let mut own: FxHashMap<String, i64> = FxHashMap::default();
        for t in patterns {
            for x in [&t.subject, &t.object] {
                if let Some(n) = name_of(x) {
                    *own.entry(n).or_default() += 1;
                }
            }
            if let NamedNodePattern::Variable(v) = &t.predicate {
                *own.entry(format!("?{}", v.as_str())).or_default() += 1;
            }
        }
        let mut bgp = Bgp {
            tbox,
            vars: Vec::new(),
            consts: Vec::new(),
        };
        let mut var_ix: FxHashMap<String, u32> = FxHashMap::default();
        let mut const_ix: FxHashMap<Term, u32> = FxHashMap::default();
        let mut term = |x: &TermPattern, bgp: &mut Bgp| -> Option<T> {
            match x {
                TermPattern::Variable(_) | TermPattern::BlankNode(_) => {
                    let n = name_of(x).expect("a variable or a blank node");
                    let distinguished = matches!(x, TermPattern::Variable(_))
                        && (self.all_distinguished
                            || self.counts.get(&n).copied().unwrap_or(0)
                                > own.get(&n).copied().unwrap_or(0));
                    let ix = *var_ix.entry(n).or_insert_with(|| {
                        bgp.vars.push((x.clone(), distinguished));
                        (bgp.vars.len() - 1) as u32
                    });
                    Some(T::Var(ix))
                }
                TermPattern::NamedNode(n) => {
                    let t = Term::NamedNode(n.clone());
                    Some(T::Const(*const_ix.entry(t.clone()).or_insert_with(|| {
                        bgp.consts.push(t);
                        (bgp.consts.len() - 1) as u32
                    })))
                }
                TermPattern::Literal(l) => {
                    let t = Term::Literal(l.clone());
                    Some(T::Const(*const_ix.entry(t.clone()).or_insert_with(|| {
                        bgp.consts.push(t);
                        (bgp.consts.len() - 1) as u32
                    })))
                }
                #[allow(unreachable_patterns)]
                _ => None,
            }
        };
        let mut atoms = Vec::with_capacity(patterns.len());
        let mut affected = false;
        for t in patterns {
            let NamedNodePattern::NamedNode(p) = &t.predicate else {
                return Ok(Outcome::Derive("a pattern has a variable predicate".into()));
            };
            let (Some(s), Some(o)) = (term(&t.subject, &mut bgp), term(&t.object, &mut bgp)) else {
                return Ok(Outcome::Derive("a pattern has an RDF-star term".into()));
            };
            if p.as_ref() == oxrdf::vocab::rdf::TYPE {
                let TermPattern::NamedNode(c) = &t.object else {
                    return Ok(Outcome::Derive(
                        "a type pattern has a variable or literal class".into(),
                    ));
                };
                if reserved(c.as_str()) {
                    return Ok(Outcome::Derive(format!(
                        "a type pattern names the class <{}> of the RDF, RDFS or OWL vocabulary",
                        c.as_str()
                    )));
                }
                let sym = tbox.lookup(c.as_str());
                affected |= sym.is_some_and(|a| tbox.affects_class(a));
                atoms.push(Atom::C(sym.unwrap_or(u32::MAX), s));
            } else {
                if reserved(p.as_str()) {
                    return Ok(Outcome::Derive(format!(
                        "a pattern names the property <{}> of the RDF, RDFS or OWL vocabulary",
                        p.as_str()
                    )));
                }
                let sym = tbox.lookup(p.as_str());
                affected |= sym.is_some_and(|r| tbox.affects_role(r));
                atoms.push(Atom::R(sym.unwrap_or(u32::MAX), s, o));
            }
        }
        if !affected {
            return Ok(Outcome::Unchanged);
        }
        // a symbol the TBox does not know has no inclusion, so each gets its own
        let mut unknown: FxHashMap<NamedNode, u32> = FxHashMap::default();
        let mut extra: Vec<NamedNode> = Vec::new();
        let base = tbox.names.len() as u32;
        for (a, t) in atoms.iter_mut().zip(patterns) {
            let iri = match a {
                Atom::C(c, _) if *c == u32::MAX => match &t.object {
                    TermPattern::NamedNode(n) => n.clone(),
                    _ => unreachable!("a class atom has a constant class"),
                },
                Atom::R(p, ..) if *p == u32::MAX => match &t.predicate {
                    NamedNodePattern::NamedNode(n) => n.clone(),
                    _ => unreachable!("a property atom has a constant predicate"),
                },
                _ => continue,
            };
            let s = *unknown.entry(iri.clone()).or_insert_with(|| {
                extra.push(iri);
                base + extra.len() as u32 - 1
            });
            match a {
                Atom::C(c, _) => *c = s,
                Atom::R(p, ..) => *p = s,
            }
        }
        let q0 = Cq {
            atoms,
            binds: Vec::new(),
        };
        let start = bgp.normalize(q0.clone());
        let Some(branches) = bgp.perfect_ref(q0, self.max_union, self.stop)? else {
            return Ok(Outcome::Derive(format!(
                "its rewriting passed budget.maxUnion ({} branches)",
                self.max_union
            )));
        };
        // no inclusion applied: the pattern keeps its own form and multiplicities
        if branches.len() == 1 && branches[0] == start {
            return Ok(Outcome::Unchanged);
        }
        let iri = |s: u32| -> NamedNode {
            if s >= base {
                extra[(s - base) as usize].clone()
            } else {
                NamedNode::new_unchecked(
                    tbox.names[s as usize]
                        .clone()
                        .expect("a branch names no property of the normalization"),
                )
            }
        };
        // the distinguished variables, in the order the pattern names them
        let dvars: Vec<Variable> = bgp
            .vars
            .iter()
            .filter(|(_, d)| *d)
            .filter_map(|(x, _)| match x {
                TermPattern::Variable(v) => Some(v.clone()),
                _ => None,
            })
            .collect();
        let n = branches.len();
        let mut union: Option<GraphPattern> = None;
        for q in &branches {
            let mut names: FxHashMap<u32, TermPattern> = FxHashMap::default();
            let mut tp = |t: T, rw: &mut Self| -> TermPattern {
                match t {
                    T::Const(c) => match &bgp.consts[c as usize] {
                        Term::NamedNode(n) => TermPattern::NamedNode(n.clone()),
                        Term::Literal(l) => TermPattern::Literal(l.clone()),
                        other => unreachable!("a constant of a pattern: {other}"),
                    },
                    T::Free => TermPattern::Variable(rw.fresh_var()),
                    T::Var(v) => {
                        if bgp.distinguished(t) {
                            return bgp.vars[v as usize].0.clone();
                        }
                        names
                            .entry(v)
                            .or_insert_with(|| TermPattern::Variable(rw.fresh_var()))
                            .clone()
                    }
                }
            };
            let mut patterns = Vec::with_capacity(q.atoms.len());
            for a in &q.atoms {
                patterns.push(match *a {
                    Atom::C(c, x) => TriplePattern {
                        subject: tp(x, self),
                        predicate: NamedNodePattern::NamedNode(oxrdf::vocab::rdf::TYPE.into()),
                        object: TermPattern::NamedNode(iri(c)),
                    },
                    Atom::R(p, s, o) => TriplePattern {
                        subject: tp(s, self),
                        predicate: NamedNodePattern::NamedNode(iri(p)),
                        object: tp(o, self),
                    },
                });
            }
            let mut branch = GraphPattern::Bgp { patterns };
            for &(v, t) in &q.binds {
                let TermPattern::Variable(var) = bgp.vars[v as usize].0.clone() else {
                    continue;
                };
                let expression = match t {
                    T::Const(c) => match &bgp.consts[c as usize] {
                        Term::NamedNode(n) => Expression::NamedNode(n.clone()),
                        Term::Literal(l) => Expression::Literal(l.clone()),
                        other => unreachable!("a constant of a pattern: {other}"),
                    },
                    T::Var(w) => match &bgp.vars[w as usize].0 {
                        TermPattern::Variable(w) => Expression::Variable(w.clone()),
                        _ => continue,
                    },
                    T::Free => continue,
                };
                branch = GraphPattern::Extend {
                    inner: Box::new(branch),
                    variable: var,
                    expression,
                };
            }
            union = Some(match union {
                None => branch,
                Some(u) => GraphPattern::Union {
                    left: Box::new(u),
                    right: Box::new(branch),
                },
            });
        }
        let union = union.expect("the rewriting keeps the query it starts from");
        let gp = GraphPattern::Distinct {
            inner: Box::new(GraphPattern::Project {
                inner: Box::new(union),
                variables: dvars,
            }),
        };
        let mut counters = serde_json::Map::new();
        counters.insert("patterns".into(), patterns.len().into());
        counters.insert("branches".into(), n.into());
        self.nodes.push(PlanInfo {
            operator: "Rewrite".into(),
            description: format!(
                "OWL 2 QL rewriting of {} pattern{} into {n} branch{}",
                patterns.len(),
                if patterns.len() == 1 { "" } else { "s" },
                if n == 1 { "" } else { "es" }
            ),
            time_ms: t0.elapsed().as_secs_f64() * 1000.0,
            actual_rows: -1,
            counters: Some(counters),
            ..Default::default()
        });
        Ok(Outcome::Rewritten(gp))
    }

    fn pattern(&mut self, gp: &GraphPattern) -> Result<GraphPattern> {
        use GraphPattern as G;
        let b = |x: GraphPattern| Box::new(x);
        Ok(match gp {
            G::Bgp { patterns } if patterns.is_empty() => gp.clone(),
            G::Bgp { patterns } => match self.bgp(patterns)? {
                Outcome::Unchanged => gp.clone(),
                Outcome::Rewritten(r) => r,
                Outcome::Derive(why) => {
                    self.nodes.push(PlanInfo {
                        operator: "Rewrite".into(),
                        description: format!(
                            "not rewritten: {why}; the owl-rl demand program derives it"
                        ),
                        actual_rows: -1,
                        ..Default::default()
                    });
                    self.warnings.push(format!(
                        "a basic graph pattern was not rewritten under OWL 2 QL, since {why}, \
                         so the owl-rl demand program answered it, which may miss answers \
                         that need unnamed individuals"
                    ));
                    self.derive.push(patterns.clone());
                    gp.clone()
                }
            },
            // a property path is derived by the owl-rl demand program
            G::Path { .. } | G::Values { .. } => gp.clone(),
            // the facts of named graphs and of remote services are read as stored, as
            // query-time reasoning reads them
            G::Graph { .. } | G::Service { .. } => gp.clone(),
            G::Join { left, right } => G::Join {
                left: b(self.pattern(left)?),
                right: b(self.pattern(right)?),
            },
            G::Lateral { left, right } => G::Lateral {
                left: b(self.pattern(left)?),
                right: b(self.pattern(right)?),
            },
            G::Union { left, right } => G::Union {
                left: b(self.pattern(left)?),
                right: b(self.pattern(right)?),
            },
            G::Minus { left, right } => G::Minus {
                left: b(self.pattern(left)?),
                right: b(self.pattern(right)?),
            },
            G::SemiJoin { left, right } => G::SemiJoin {
                left: b(self.pattern(left)?),
                right: b(self.pattern(right)?),
            },
            G::AntiJoin { left, right } => G::AntiJoin {
                left: b(self.pattern(left)?),
                right: b(self.pattern(right)?),
            },
            G::LeftJoin {
                left,
                right,
                expression,
            } => G::LeftJoin {
                left: b(self.pattern(left)?),
                right: b(self.pattern(right)?),
                expression: match expression {
                    Some(e) => Some(self.expr(e)?),
                    None => None,
                },
            },
            G::Filter { expr, inner } => G::Filter {
                expr: self.expr(expr)?,
                inner: b(self.pattern(inner)?),
            },
            G::Extend {
                inner,
                variable,
                expression,
            } => G::Extend {
                inner: b(self.pattern(inner)?),
                variable: variable.clone(),
                expression: self.expr(expression)?,
            },
            G::Assign {
                inner,
                variable,
                expression,
            } => G::Assign {
                inner: b(self.pattern(inner)?),
                variable: variable.clone(),
                expression: self.expr(expression)?,
            },
            G::Unfold {
                inner,
                expression,
                variable,
                second,
            } => G::Unfold {
                inner: b(self.pattern(inner)?),
                expression: self.expr(expression)?,
                variable: variable.clone(),
                second: second.clone(),
            },
            G::OrderBy { inner, expression } => G::OrderBy {
                inner: b(self.pattern(inner)?),
                expression: expression
                    .iter()
                    .map(|o| {
                        Ok(match o {
                            OrderExpression::Asc(e) => OrderExpression::Asc(self.expr(e)?),
                            OrderExpression::Desc(e) => OrderExpression::Desc(self.expr(e)?),
                        })
                    })
                    .collect::<Result<_>>()?,
            },
            G::Project { inner, variables } => G::Project {
                inner: b(self.pattern(inner)?),
                variables: variables.clone(),
            },
            G::Distinct { inner } => G::Distinct {
                inner: b(self.pattern(inner)?),
            },
            G::Reduced { inner } => G::Reduced {
                inner: b(self.pattern(inner)?),
            },
            G::Slice {
                inner,
                start,
                length,
            } => G::Slice {
                inner: b(self.pattern(inner)?),
                start: *start,
                length: *length,
            },
            G::Group {
                inner,
                variables,
                aggregates,
            } => G::Group {
                inner: b(self.pattern(inner)?),
                variables: variables.clone(),
                aggregates: aggregates
                    .iter()
                    .map(|(v, a)| {
                        Ok((
                            v.clone(),
                            match a {
                                AggregateExpression::CountSolutions { .. } => a.clone(),
                                AggregateExpression::FunctionCall {
                                    name,
                                    expr,
                                    distinct,
                                } => AggregateExpression::FunctionCall {
                                    name: name.clone(),
                                    expr: self.expr(expr)?,
                                    distinct: *distinct,
                                },
                                AggregateExpression::Fold { .. } => {
                                    let mut fold = a.clone();
                                    if let AggregateExpression::Fold { expr, value, .. } = &mut fold
                                    {
                                        *expr = self.expr(expr)?;
                                        if let Some(v) = value {
                                            *v = self.expr(v)?;
                                        }
                                    }
                                    fold
                                }
                            },
                        ))
                    })
                    .collect::<Result<_>>()?,
            },
        })
    }

    /// The expression with the patterns of its `EXISTS` rewritten.
    fn expr(&mut self, e: &Expression) -> Result<Expression> {
        use Expression as E;
        let b = |x: Expression| Box::new(x);
        Ok(match e {
            E::Exists(p) => E::Exists(Box::new(self.pattern(p)?)),
            E::Or(x, y) => E::Or(b(self.expr(x)?), b(self.expr(y)?)),
            E::And(x, y) => E::And(b(self.expr(x)?), b(self.expr(y)?)),
            E::Equal(x, y) => E::Equal(b(self.expr(x)?), b(self.expr(y)?)),
            E::SameTerm(x, y) => E::SameTerm(b(self.expr(x)?), b(self.expr(y)?)),
            E::Greater(x, y) => E::Greater(b(self.expr(x)?), b(self.expr(y)?)),
            E::GreaterOrEqual(x, y) => E::GreaterOrEqual(b(self.expr(x)?), b(self.expr(y)?)),
            E::Less(x, y) => E::Less(b(self.expr(x)?), b(self.expr(y)?)),
            E::LessOrEqual(x, y) => E::LessOrEqual(b(self.expr(x)?), b(self.expr(y)?)),
            E::Add(x, y) => E::Add(b(self.expr(x)?), b(self.expr(y)?)),
            E::Subtract(x, y) => E::Subtract(b(self.expr(x)?), b(self.expr(y)?)),
            E::Multiply(x, y) => E::Multiply(b(self.expr(x)?), b(self.expr(y)?)),
            E::Divide(x, y) => E::Divide(b(self.expr(x)?), b(self.expr(y)?)),
            E::In(x, l) => E::In(
                b(self.expr(x)?),
                l.iter().map(|y| self.expr(y)).collect::<Result<_>>()?,
            ),
            E::UnaryPlus(x) => E::UnaryPlus(b(self.expr(x)?)),
            E::UnaryMinus(x) => E::UnaryMinus(b(self.expr(x)?)),
            E::Not(x) => E::Not(b(self.expr(x)?)),
            E::If(x, y, z) => E::If(b(self.expr(x)?), b(self.expr(y)?), b(self.expr(z)?)),
            E::Coalesce(l) => E::Coalesce(l.iter().map(|y| self.expr(y)).collect::<Result<_>>()?),
            E::FunctionCall(f, l) => E::FunctionCall(
                f.clone(),
                l.iter().map(|y| self.expr(y)).collect::<Result<_>>()?,
            ),
            E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => e.clone(),
        })
    }
}

/// `query` rewritten under `tbox`, with at most `max_union` branches per basic graph
/// pattern. A pattern that no inclusion changes stays as it is, and the result's
/// `derive` lists the patterns left for the `owl-rl` demand program. `None` for a
/// DESCRIBE, which query-time reasoning refuses before it rewrites.
pub fn rewrite_query(
    query: &Query,
    tbox: &Tbox,
    max_union: usize,
    stop: &dyn Fn() -> Result<()>,
) -> Result<Option<Rewritten>> {
    // the projection that ASK wraps its pattern in names every variable, and none of them
    // is an answer, so it does not count
    let sse = match query {
        Query::Ask {
            pattern: GraphPattern::Project { inner, .. },
            dataset,
            base_iri,
        } => Query::Ask {
            pattern: (**inner).clone(),
            dataset: dataset.clone(),
            base_iri: base_iri.clone(),
        }
        .to_sse(),
        q => q.to_sse(),
    };
    let mut counts = FxHashMap::default();
    count_names(&sse, &mut counts, 1);
    let mut rw = Rewriter {
        tbox,
        max_union,
        stop,
        counts,
        all_distinguished: sse.contains("(count)") || sse.contains("(count distinct)"),
        fresh: 0,
        derive: Vec::new(),
        nodes: Vec::new(),
        warnings: tbox.warnings(),
    };
    let rewrite = |p: &GraphPattern, rw: &mut Rewriter| rw.pattern(p);
    let query = match query {
        Query::Select {
            dataset,
            pattern,
            base_iri,
        } => Query::Select {
            dataset: dataset.clone(),
            pattern: rewrite(pattern, &mut rw)?,
            base_iri: base_iri.clone(),
        },
        Query::Ask {
            dataset,
            pattern,
            base_iri,
        } => Query::Ask {
            dataset: dataset.clone(),
            pattern: rewrite(pattern, &mut rw)?,
            base_iri: base_iri.clone(),
        },
        Query::Construct {
            template,
            graph_templates,
            dataset,
            pattern,
            base_iri,
        } => Query::Construct {
            template: template.clone(),
            graph_templates: graph_templates.clone(),
            dataset: dataset.clone(),
            pattern: rewrite(pattern, &mut rw)?,
            base_iri: base_iri.clone(),
        },
        // query-time reasoning refuses DESCRIBE before it rewrites
        Query::Describe { .. } => return Ok(None),
    };
    Ok(Some(Rewritten {
        query,
        derive: rw.derive,
        nodes: rw.nodes,
        warnings: rw.warnings,
    }))
}

#[cfg(test)]
mod tests;
