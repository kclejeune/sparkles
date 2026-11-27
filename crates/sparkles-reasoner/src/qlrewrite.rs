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
use sparkles_core::sparql::reasoning::{GuardedRewritten, Rewritten, SessionRequest};
use sparkles_core::store::{Chunk, Snapshot};
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
        Tbox::from_triples_checked(triples, &|| Ok(())).expect("no stop function fails")
    }

    /// [`from_triples`](Self::from_triples), which checks `stop` between its phases and
    /// while it maps the axioms.
    fn from_triples_checked(
        triples: Vec<oxrdf::Triple>,
        stop: &dyn Fn() -> Result<()>,
    ) -> Result<Tbox> {
        stop()?;
        let ontology = sparkles_owl::read::read_graph(
            triples,
            sparkles_owl::ReadOptions {
                terminology_only: true,
                ..Default::default()
            },
        );
        stop()?;
        let part =
            sparkles_owl::profiles::partition(&ontology, sparkles_owl::profiles::Profile::Ql);
        stop()?;
        let mut t = Tbox {
            unmapped: ontology.unmapped.len(),
            ..Default::default()
        };
        let outside = part.outside_axioms();
        t.outside_count = outside.len();
        for v in part.outside.iter().take(MAX_LISTED) {
            t.outside.push(v.message.clone());
        }
        for (i, ax) in part.inside.iter().enumerate() {
            if i % 256 == 255 {
                stop()?;
            }
            t.axioms += 1;
            t.add_axiom(&ax.kind);
        }
        stop()?;
        Ok(t)
    }

    /// The TBox of the terminology in `graphs` of `snap`: the triples of the structural
    /// predicates of OWL and RDFS and the `rdf:type` triples of their structural types.
    /// The triples read, about [`TRIPLE_BYTES`] each while the axioms are parsed, are
    /// charged to `budget`.
    pub(crate) fn read(
        snap: &Snapshot,
        graphs: &[Id],
        stop: &dyn Fn() -> Result<()>,
        budget: &Arc<crate::budget::Budget>,
    ) -> Result<Tbox> {
        // Declared before `triples` so payload drops before held credit returns.
        let mut held = crate::budget::Held::new(budget);
        let mut triples = Vec::new();
        let (mut visited, mut kept) = (0usize, 0usize);
        // each key as the scan visits it: the stop function and the budget are checked
        // as the scan goes, and only the triples of the input graphs are charged
        let mut each = |k: sparkles_core::index::Key| -> Result<()> {
            visited += 1;
            if visited.is_multiple_of(4096) {
                stop()?;
            }
            let q = Perm::Pos.to_quad(&k);
            if !graphs.contains(&q[3]) {
                return Ok(());
            }
            if kept.is_multiple_of(1024) {
                held.try_grow(1024 * TRIPLE_BYTES)
                    .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL axioms"))?;
            }
            kept += 1;
            let (Some(s), Some(Term::NamedNode(p)), Some(o)) =
                (snap.term(q[0]), snap.term(q[1]), snap.term(q[2]))
            else {
                return Ok(());
            };
            let s = match s {
                Term::NamedNode(n) => oxrdf::NamedOrBlankNode::NamedNode(n),
                Term::BlankNode(b) => oxrdf::NamedOrBlankNode::BlankNode(b),
                _ => return Ok(()),
            };
            triples.push(oxrdf::Triple::new(s, p, o));
            Ok(())
        };
        let mut scan = |prefix: &[u64]| -> Result<()> {
            snap.scan(Perm::Pos, prefix, |c| {
                match c {
                    Chunk::Block(b, s, e) => {
                        for i in s..e {
                            each(b.key(i))?;
                        }
                    }
                    Chunk::Row(k) => each(k)?,
                }
                Ok(true)
            })
        };
        for p in sparkles_owl::vocab::STRUCTURAL_PREDICATES {
            if let Some(id) = snap.lookup_iri(p) {
                scan(&[id.0])?;
            }
        }
        if let Some(ty) = snap.lookup_iri(oxrdf::vocab::rdf::TYPE.as_str()) {
            for c in sparkles_owl::vocab::STRUCTURAL_TYPES {
                if let Some(id) = snap.lookup_iri(c) {
                    scan(&[ty.0, id.0])?;
                }
            }
        }
        stop()?;
        Tbox::from_triples_checked(triples, stop)
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

/// An active OWL 2 QL TBox whose estimated memory stays charged to the calling budget
/// until this guard drops. Declared with `tbox` before `_held` so payload drops first.
pub struct ChargedTbox {
    pub(crate) tbox: Arc<Tbox>,
    _held: crate::budget::Held,
}

impl std::ops::Deref for ChargedTbox {
    type Target = Tbox;

    fn deref(&self) -> &Tbox {
        &self.tbox
    }
}

/// The TBox of the input graphs of `inputs` that the caller of `req` reads, from the
/// TBoxes kept for the same generation, commit and view, or read from the snapshot,
/// admitted against `budget` on both cache hits and misses.
pub(crate) fn tbox_charged(
    req: &SessionRequest,
    inputs: &Inputs,
    budget: &Arc<crate::budget::Budget>,
) -> Result<ChargedTbox> {
    let snap = &req.snap;
    let resolved = match &req.readable {
        Some(access) if !access.reads_all() => {
            inputs::resolve_readable_iri(snap, inputs, &|g| match g {
                inputs::GraphRef::Default => access.read.default_graph(),
                inputs::GraphRef::Named(iri) => access.read.allows_named_iri(iri),
            })
        }
        _ => inputs::resolve(snap, inputs),
    };
    let graphs = match &req.readable {
        Some(access) if !access.reads_all() => resolved.readable_ids(snap, &|g| match g {
            inputs::GraphRef::Default => access.read.default_graph(),
            inputs::GraphRef::Named(iri) => access.read.allows_named_iri(iri),
        }),
        _ => resolved.ids(snap),
    };
    let key = (snap.cacheable && !req.no_cache).then(|| {
        format!(
            "{}|{}|{}|{:?}",
            snap.generation.uid,
            snap.commit,
            snap.mask_key().unwrap_or(""),
            graphs
        )
    });
    if let Some(k) = &key {
        let hit = {
            let mut kept = TBOXES.lock();
            kept.iter().position(|(x, _)| x == k).map(|i| {
                let e = kept.remove(i);
                let t = e.1.clone();
                kept.push(e);
                t
            })
        };
        if let Some(t) = hit {
            let _held = crate::budget::Held::try_new(budget, t.approx_bytes())
                .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL axioms"))?;
            return Ok(ChargedTbox { tbox: t, _held });
        }
    }
    // the TBox and the triples it is read from live while the query plans, so the
    // query's memory budget bounds them
    let t = Arc::new(Tbox::read(snap, &graphs, &*req.stop, budget)?);
    let _held = crate::budget::Held::try_new(budget, t.approx_bytes())
        .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL axioms"))?;
    if let Some(k) = key {
        let evicted = {
            let mut kept = TBOXES.lock();
            kept.push((k, t.clone()));
            (kept.len() > KEPT_TBOXES).then(|| kept.remove(0))
        };
        drop(evicted);
    }
    Ok(ChargedTbox { tbox: t, _held })
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

impl Cq {
    /// The variables of the pattern that the query gives a value: those its atoms hold,
    /// and those bound to a constant or to a variable its atoms hold.
    fn bound_vars(&self) -> FxHashSet<u32> {
        let mut out: FxHashSet<u32> = FxHashSet::default();
        for a in &self.atoms {
            for t in a.terms() {
                if let T::Var(v) = t {
                    out.insert(v);
                }
            }
        }
        let held = out.clone();
        for &(v, t) in &self.binds {
            match t {
                T::Const(_) => {
                    out.insert(v);
                }
                T::Var(w) if held.contains(&w) => {
                    out.insert(v);
                }
                _ => {}
            }
        }
        out
    }
}

/// The variables and constants of one basic graph pattern.
struct Bgp<'a> {
    tbox: &'a Tbox,
    /// each variable as the query wrote it, and whether it is distinguished
    vars: Vec<(TermPattern, bool)>,
    consts: Vec<Term>,
    /// the values of the variables of the query's initial bindings
    values: FxHashMap<u32, Term>,
}

impl Bgp<'_> {
    /// Atoms and bindings sorted and each once. A variable keeps its name, so a branch
    /// gives the query's variables the values its stored facts bind them to.
    fn normalize(&self, mut q: Cq) -> Cq {
        q.atoms.sort_unstable();
        q.atoms.dedup();
        q.binds.sort_unstable();
        q.binds.dedup();
        q
    }

    /// The terms of `q` that an existential inclusion can stand for (`_` of PerfectRef):
    /// an unnamed term, or a non-distinguished variable that one atom of `q` holds once.
    fn unbound_terms(&self, q: &Cq) -> FxHashSet<T> {
        let mut count: FxHashMap<u32, u32> = FxHashMap::default();
        for a in &q.atoms {
            for t in a.terms() {
                if let T::Var(v) = t {
                    *count.entry(v).or_default() += 1;
                }
            }
        }
        let mut out: FxHashSet<T> = count
            .into_iter()
            .filter(|&(v, n)| n == 1 && !self.vars[v as usize].1)
            .map(|(v, _)| T::Var(v))
            .collect();
        out.insert(T::Free);
        out
    }

    /// The atoms that imply `a` under one inclusion (`gr` of PerfectRef), where `unbound`
    /// holds the terms of the query that an existential can stand for.
    fn implied(&self, a: Atom, unbound: &FxHashSet<T>, out: &mut Vec<Atom>) {
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
                if unbound.contains(&o) {
                    let e = Basic::Exists(Role { p, inv: false });
                    for &b in t.concepts.get(&e).into_iter().flatten() {
                        from_basic(b, s, out);
                    }
                }
                if unbound.contains(&s) {
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
                (T::Var(v), c @ T::Const(k)) | (c @ T::Const(k), T::Var(v)) => {
                    // a variable of the initial bindings unifies only with its own value,
                    // which the planner substitutes, so it needs no binding
                    if let Some(value) = self.values.get(&v) {
                        if *value != self.consts[k as usize] {
                            return None;
                        }
                        sub.insert(v, c);
                        continue;
                    }
                    // every variable of the query keeps its value in the branch
                    sub.insert(v, c);
                    binds.push((v, c));
                }
                (T::Var(v), T::Var(w)) => {
                    let (bv, bw) = (self.values.get(&v), self.values.get(&w));
                    if let (Some(a), Some(b)) = (bv, bw) {
                        if a != b {
                            return None;
                        }
                        // both take the same value from the planner
                        sub.insert(v, T::Var(w));
                        continue;
                    }
                    // bind a variable without an initial value to one with it, and a
                    // non-distinguished variable rather than a distinguished one
                    let (from, to) = if bw.is_some() {
                        (v, w)
                    } else if bv.is_some() || !self.vars[w as usize].1 {
                        (w, v)
                    } else {
                        (v, w)
                    };
                    sub.insert(from, T::Var(to));
                    binds.push((from, T::Var(to)));
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

    /// PerfectRef from `q0`, or `None` when the rewriting passes `max` queries. The
    /// queries it keeps are charged to `budget` while it runs; on success the surviving
    /// queries' credit is returned in `Held` so they stay charged while rendered, and on
    /// bound overflow or error all credit is released on unwind/return.
    fn perfect_ref(
        &self,
        q0: Cq,
        max: usize,
        stop: &dyn Fn() -> Result<()>,
        budget: &Arc<crate::budget::Budget>,
    ) -> Result<Option<(Vec<Cq>, crate::budget::Held)>> {
        let mut held = crate::budget::Held::new(budget);
        let Some(out) = self.perfect_ref_charged(q0, max, stop, &mut held)? else {
            return Ok(None);
        };
        let kept_bytes = out
            .iter()
            .fold(0usize, |acc, q| acc.saturating_add(cq_bytes(q)));
        held.shrink(held.bytes().saturating_sub(kept_bytes));
        Ok(Some((out, held)))
    }

    fn perfect_ref_charged(
        &self,
        q0: Cq,
        max: usize,
        stop: &dyn Fn() -> Result<()>,
        held: &mut crate::budget::Held,
    ) -> Result<Option<Vec<Cq>>> {
        let q0 = self.normalize(q0);
        let mut seen: FxHashSet<Cq> = FxHashSet::default();
        let mut order: Vec<Cq> = Vec::new();
        let n = 2 * cq_bytes(&q0);
        held.try_grow(n)
            .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL rewriting"))?;
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
            // each query is kept as it is made, so the bound and the stop function hold
            // while one query's replacements and reductions are made
            // a query kept twice, in `seen` and `order`, is charged to the query's memory
            let mut add = |f: Cq, order: &mut Vec<Cq>| -> Result<bool> {
                if !seen.contains(&f) {
                    let n = 2 * cq_bytes(&f);
                    held.try_grow(n)
                        .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL rewriting"))?;
                    seen.insert(f.clone());
                    order.push(f);
                    if order.len() > max {
                        return Ok(false);
                    }
                }
                Ok(true)
            };
            // every replacement and pair tried counts, unified or not
            let mut tried = 0usize;
            let mut attempt = || -> Result<()> {
                tried += 1;
                if tried.is_multiple_of(256) {
                    stop()?;
                }
                Ok(())
            };
            let unbound = self.unbound_terms(&q);
            for i in 0..q.atoms.len() {
                implied.clear();
                self.implied(q.atoms[i], &unbound, &mut implied);
                for &a in &implied {
                    attempt()?;
                    let mut atoms = q.atoms.clone();
                    atoms[i] = a;
                    let f = self.normalize(Cq {
                        atoms,
                        binds: q.binds.clone(),
                    });
                    if !add(f, &mut order)? {
                        return Ok(None);
                    }
                }
            }
            for i in 0..q.atoms.len() {
                for j in i + 1..q.atoms.len() {
                    attempt()?;
                    if let Some(r) = self.reduce(&q, i, j)
                        && !add(r, &mut order)?
                    {
                        return Ok(None);
                    }
                }
            }
        }
        // a branch that names a property of the normalization matches no stored fact, and
        // a branch whose atoms hold another's, with the same bindings and the same
        // variables, adds no answer
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
        let vars: Vec<FxHashSet<u32>> = order.iter().map(|q| q.bound_vars()).collect();
        let keep: Vec<bool> = (0..order.len())
            .map(|i| {
                !(0..order.len()).any(|j| {
                    j != i
                        && order[j].binds == order[i].binds
                        && vars[j] == vars[i]
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

/// An estimate of the bytes one query of the rewriting holds.
fn cq_bytes(q: &Cq) -> usize {
    q.atoms.len() * std::mem::size_of::<Atom>()
        + q.binds.len() * std::mem::size_of::<(u32, T)>()
        + std::mem::size_of::<Cq>()
        + 32
}

/// The count of each variable and blank node in the SSE form of `text`, by name with its
/// sigil. A name inside a string literal counts too, which errs towards distinguished.
fn count_names(
    text: &str,
    out: &mut FxHashMap<String, i64>,
    held: &mut crate::budget::Held,
) -> Result<()> {
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
            count_name(&text[start..i], out, held)?;
        }
    }
    Ok(())
}

fn count_name(
    name: &str,
    out: &mut FxHashMap<String, i64>,
    held: &mut crate::budget::Held,
) -> Result<()> {
    if let Some(n) = out.get_mut(name) {
        *n += 1;
    } else {
        held.try_grow(name.len() + 96)
            .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL rewriting"))?;
        out.insert(name.to_owned(), 1);
    }
    Ok(())
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

/// One branch of a rewritten pattern: its triple patterns, and the values unification
/// gave the pattern's variables.
struct Rendered {
    patterns: Vec<TriplePattern>,
    binds: Vec<(u32, Expression)>,
    /// the bytes of the names of the variables the bindings give a value
    bind_names: usize,
}

impl Rendered {
    /// An estimate of the bytes a copy of the branch holds, with its terms' strings.
    fn bytes(&self) -> usize {
        fn term(t: &TermPattern) -> usize {
            match t {
                TermPattern::NamedNode(n) => n.as_str().len(),
                TermPattern::Literal(l) => {
                    l.value().len() + l.datatype().as_str().len() + l.language().map_or(0, str::len)
                }
                TermPattern::Variable(v) => v.as_str().len(),
                TermPattern::BlankNode(b) => b.as_str().len(),
                #[allow(unreachable_patterns)]
                _ => 64,
            }
        }
        fn expr(e: &Expression) -> usize {
            match e {
                Expression::NamedNode(n) => n.as_str().len(),
                Expression::Literal(l) => {
                    l.value().len() + l.datatype().as_str().len() + l.language().map_or(0, str::len)
                }
                Expression::Variable(v) => v.as_str().len(),
                _ => 64,
            }
        }
        let patterns: usize = self
            .patterns
            .iter()
            .map(|t| {
                let p = match &t.predicate {
                    NamedNodePattern::NamedNode(n) => n.as_str().len(),
                    NamedNodePattern::Variable(v) => v.as_str().len(),
                };
                term(&t.subject) + p + term(&t.object) + 3 * 48
            })
            .sum();
        let binds: usize = self.binds.iter().map(|(_, e)| expr(e) + 128).sum();
        patterns + binds + self.bind_names + 64
    }
}

// The prefix plus the largest decimal usize on supported targets.
const FRESH_NAME_BYTES: usize = "__ql".len() + 20;

/// Size the heap owned by a temporary branch and its group key, excluding the
/// slots in the group vectors.
fn estimate_rendered_bytes(
    q: &Cq,
    consts: &[Term],
    names: &[Variable],
    iri: impl Fn(u32) -> usize,
) -> usize {
    let term_len = |t: T| -> usize {
        match t {
            T::Const(c) => match &consts[c as usize] {
                Term::NamedNode(n) => n.as_str().len(),
                Term::Literal(l) => {
                    l.value().len() + l.datatype().as_str().len() + l.language().map_or(0, str::len)
                }
                _ => 64,
            },
            T::Free => FRESH_NAME_BYTES,
            T::Var(v) => names[v as usize].as_str().len(),
        }
    };
    let pattern_slot = size_of::<TriplePattern>().max(3 * 48);
    let binding_slot = size_of::<(u32, Expression)>().max(128);
    let rdf_type_len = oxrdf::vocab::rdf::TYPE.as_str().len();
    let patterns: usize = q.atoms.iter().fold(0usize, |acc, a| {
        let atom_bytes = match *a {
            Atom::C(c, x) => term_len(x)
                .saturating_add(rdf_type_len)
                .saturating_add(iri(c))
                .saturating_add(pattern_slot),
            Atom::R(p, s, o) => term_len(s)
                .saturating_add(iri(p))
                .saturating_add(term_len(o))
                .saturating_add(pattern_slot),
        };
        acc.saturating_add(atom_bytes)
    });
    let mut binds: usize = 0;
    let mut bind_names: usize = 0;
    for &(v, t) in &q.binds {
        // The binding vector reserves a slot even for a binding that gives no value.
        binds = binds.saturating_add(binding_slot);
        if t != T::Free {
            binds = binds.saturating_add(term_len(t));
            bind_names = bind_names.saturating_add(names[v as usize].as_str().len());
        }
    }
    let set_bytes = q
        .atoms
        .len()
        .saturating_mul(2)
        .saturating_add(q.binds.len())
        .saturating_mul(size_of::<u32>());
    patterns
        .saturating_add(binds)
        .saturating_add(bind_names)
        .saturating_add(set_bytes)
}

/// Admit the new buffer alongside the old one before growing a group vector.
fn reserve_group_slot<T>(vec: &mut Vec<T>, held: &mut crate::budget::Held) -> Result<()> {
    let old = vec.capacity();
    let elem = size_of::<T>();
    let next = sparkles_budget::sizes::vec_grown(old, vec.len(), 1, elem);
    if next > old {
        held.try_grow(next.saturating_mul(elem))
            .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL rewriting"))?;
        vec.reserve_exact(next - vec.len());
        held.shrink(old * elem);
    }
    Ok(())
}

/// Build the sorted, deduplicated list of bound variables of `q` into a pre-sized `Vec`
/// without allocating intermediate hash sets.
fn bound_vars_sorted(q: &Cq) -> Vec<u32> {
    let mut out = Vec::with_capacity(
        q.atoms
            .len()
            .saturating_mul(2)
            .saturating_add(q.binds.len()),
    );
    for a in &q.atoms {
        for t in a.terms() {
            if let T::Var(v) = t {
                out.push(v);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    let atom_var_count = out.len();
    for &(v, t) in &q.binds {
        match t {
            T::Const(_) => out.push(v),
            T::Var(w) if out[..atom_var_count].binary_search(&w).is_ok() => out.push(v),
            _ => {}
        }
    }
    out.sort_unstable();
    out.dedup();
    out
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
    /// the values of the query's initial bindings, by variable name
    bound: FxHashMap<String, Term>,
    /// the occurrences of each name in the whole query
    counts: FxHashMap<String, i64>,
    /// every variable is distinguished (`COUNT(*)`)
    all_distinguished: bool,
    fresh: usize,
    /// a property path's warning has been given
    path_warned: bool,
    derive: Vec<Vec<TriplePattern>>,
    nodes: Vec<PlanInfo>,
    warnings: Vec<String>,
    /// charge for emitted rewritten algebra (first branch copies and wider-group filters);
    /// drops before `budget`
    emitted_held: crate::budget::Held,
    /// the query's memory budget, declared last so buffers and `Held` drop before parent credit returns
    budget: Arc<crate::budget::Budget>,
}

impl Rewriter<'_> {
    /// A variable the query does not name.
    fn fresh_var(&mut self) -> Variable {
        loop {
            self.fresh += 1;
            let name = format!("__ql{}", self.fresh);
            if !self.counts.contains_key(&format!("?{name}")) {
                return Variable::new_unchecked(name);
            }
        }
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
            values: FxHashMap::default(),
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
        for (i, (x, _)) in bgp.vars.iter().enumerate() {
            if let TermPattern::Variable(v) = x
                && let Some(value) = self.bound.get(v.as_str())
            {
                bgp.values.insert(i as u32, value.clone());
            }
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
        let Some((branches, branches_held)) =
            bgp.perfect_ref(q0, self.max_union, self.stop, &self.budget)?
        else {
            return Ok(Outcome::Derive(format!(
                "its rewriting passed budget.maxUnion ({} branches)",
                self.max_union
            )));
        };
        // Bind `branches_held` before `branches` so `branches` drops before credit returns.
        let (branches_held, branches) = (branches_held, branches);
        // no inclusion applied: the pattern keeps its own form and multiplicities
        if branches.len() == 1 && branches[0] == start {
            return Ok(Outcome::Unchanged);
        }
        let iri = |s: u32| -> &str {
            if s >= base {
                extra[(s - base) as usize].as_str()
            } else {
                tbox.names[s as usize]
                    .as_deref()
                    .expect("a branch names no property of the normalization")
            }
        };
        // The branches share names for the query's variables and for blank nodes.
        let names_bytes = bgp.vars.iter().fold(0usize, |bytes, (term, _)| {
            let len = match term {
                TermPattern::Variable(v) => v.as_str().len(),
                _ => FRESH_NAME_BYTES,
            };
            bytes.saturating_add(size_of::<Variable>().saturating_add(len))
        });
        let mut names_held = crate::budget::Held::try_new(&self.budget, names_bytes)
            .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL rewriting"))?;
        let mut names = Vec::with_capacity(bgp.vars.len());
        let mut visible_len = 0;
        let mut visible_bytes = 0usize;
        for (x, _) in &bgp.vars {
            names.push(match x {
                TermPattern::Variable(v) => {
                    visible_len += 1;
                    visible_bytes =
                        visible_bytes.saturating_add(size_of::<Variable>() + v.as_str().len());
                    v.clone()
                }
                _ => self.fresh_var(),
            });
        }
        let n = branches.len();
        // the branches grouped by the variables they give a value: a branch that gives
        // every variable one answers as the stored and derived facts would, and one that
        // leaves some to unnamed individuals answers only where no branch that names more
        // of them gives the same values to the ones it names.
        // `groups_held` is declared before `groups` so temporary rendered branches drop
        // before their credit returns.
        let mut groups_held = crate::budget::Held::new(&self.budget);
        let mut groups: Vec<(Vec<u32>, Vec<Rendered>)> = Vec::new();
        for q in &branches {
            let est_bytes = estimate_rendered_bytes(q, &bgp.consts, &names, |s| iri(s).len());
            groups_held
                .try_grow(est_bytes)
                .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL rewriting"))?;
            let tp = |t: T, rw: &mut Self| -> TermPattern {
                match t {
                    T::Const(c) => match &bgp.consts[c as usize] {
                        Term::NamedNode(n) => TermPattern::NamedNode(n.clone()),
                        Term::Literal(l) => TermPattern::Literal(l.clone()),
                        other => unreachable!("a constant of a pattern: {other}"),
                    },
                    T::Free => TermPattern::Variable(rw.fresh_var()),
                    T::Var(v) => TermPattern::Variable(names[v as usize].clone()),
                }
            };
            let mut patterns = Vec::with_capacity(q.atoms.len());
            for a in &q.atoms {
                patterns.push(match *a {
                    Atom::C(c, x) => TriplePattern {
                        subject: tp(x, self),
                        predicate: NamedNodePattern::NamedNode(oxrdf::vocab::rdf::TYPE.into()),
                        object: TermPattern::NamedNode(NamedNode::new_unchecked(iri(c))),
                    },
                    Atom::R(p, s, o) => TriplePattern {
                        subject: tp(s, self),
                        predicate: NamedNodePattern::NamedNode(NamedNode::new_unchecked(iri(p))),
                        object: tp(o, self),
                    },
                });
            }
            let mut binds: Vec<(u32, Expression)> = Vec::with_capacity(q.binds.len());
            for &(v, t) in &q.binds {
                let expression = match t {
                    T::Const(c) => match &bgp.consts[c as usize] {
                        Term::NamedNode(n) => Expression::NamedNode(n.clone()),
                        Term::Literal(l) => Expression::Literal(l.clone()),
                        other => unreachable!("a constant of a pattern: {other}"),
                    },
                    T::Var(w) => Expression::Variable(names[w as usize].clone()),
                    T::Free => continue,
                };
                binds.push((v, expression));
            }
            let set = bound_vars_sorted(q);
            let bind_names = binds
                .iter()
                .map(|(v, _)| names[*v as usize].as_str().len())
                .sum();
            let b = Rendered {
                patterns,
                binds,
                bind_names,
            };
            match groups.iter_mut().find(|(s, _)| *s == set) {
                Some((_, g)) => {
                    let key_bytes = set.capacity() * size_of::<u32>();
                    drop(set);
                    groups_held.shrink(key_bytes);
                    reserve_group_slot(g, &mut groups_held)?;
                    g.push(b);
                }
                None => {
                    reserve_group_slot(&mut groups, &mut groups_held)?;
                    groups_held
                        .try_grow(size_of::<Rendered>())
                        .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL rewriting"))?;
                    groups.push((set, vec![b]));
                }
            }
        }
        drop(branches);
        drop(branches_held);
        // a branch's bindings: `BIND` for a variable the branch gives its value, and a
        // test of the value for one that the row of an outer `NOT EXISTS` already holds,
        // since a substituted variable cannot be bound again
        let render = |b: &Rendered, outer: &[u32]| -> GraphPattern {
            let mut gp = GraphPattern::Bgp {
                patterns: b.patterns.clone(),
            };
            for (v, e) in &b.binds {
                let var = names[*v as usize].clone();
                gp = if outer.contains(v) {
                    GraphPattern::Filter {
                        expr: Expression::SameTerm(
                            Box::new(Expression::Variable(var)),
                            Box::new(e.clone()),
                        ),
                        inner: Box::new(gp),
                    }
                } else {
                    GraphPattern::Extend {
                        inner: Box::new(gp),
                        variable: var,
                        expression: e.clone(),
                    }
                };
            }
            gp
        };
        let union_of = |ps: Vec<GraphPattern>| {
            ps.into_iter().reduce(|l, r| GraphPattern::Union {
                left: Box::new(l),
                right: Box::new(r),
            })
        };
        // the algebra the wider groups repeat is bounded like the branches: past
        // `budget.maxUnion` branch copies the pattern falls back, and each copy is charged
        let mut copies = 0usize;
        for (set, _) in &groups {
            for (s, g) in &groups {
                if s.len() > set.len() && set.iter().all(|v| s.contains(v)) {
                    copies += g.len();
                }
            }
        }
        if copies > self.max_union {
            return Ok(Outcome::Derive(format!(
                "the rows it leaves to unnamed individuals need {copies} copies of its \
                 branches, past budget.maxUnion ({})",
                self.max_union
            )));
        }
        // Admit both the first emitted copy of every branch and every wider-group
        // `NOT EXISTS` copy while `groups_held` stays charged for the overlapping
        // temporary `Rendered` state.
        let mut bytes = 0usize;
        for (set, group) in &groups {
            bytes = bytes.saturating_add(group.iter().map(Rendered::bytes).sum::<usize>());
            for (s, g) in &groups {
                if s.len() > set.len() && set.iter().all(|v| s.contains(v)) {
                    bytes = bytes.saturating_add(g.iter().map(Rendered::bytes).sum::<usize>());
                }
            }
        }
        self.emitted_held
            .try_grow(bytes)
            .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL rewriting"))?;
        let mut parts = Vec::with_capacity(groups.len());
        for (set, group) in &groups {
            (self.stop)()?;
            let own = union_of(group.iter().map(|b| render(b, &[])).collect())
                .expect("a group has a branch");
            // the groups that name every variable of this one and more
            let wider: Vec<GraphPattern> = groups
                .iter()
                .filter(|(s, _)| s.len() > set.len() && set.iter().all(|v| s.contains(v)))
                .flat_map(|(_, g)| g.iter().map(|b| render(b, set)))
                .collect();
            parts.push(match union_of(wider) {
                None => own,
                // NOT EXISTS reads the wider groups with this row's values, so a row that
                // names nothing is dropped whenever a wider group has any row
                Some(w) => GraphPattern::Filter {
                    expr: Expression::Not(Box::new(Expression::Exists(Box::new(w)))),
                    inner: Box::new(own),
                },
            });
        }
        drop(groups);
        drop(groups_held);
        let union = union_of(parts).expect("the rewriting keeps the query it starts from");
        self.emitted_held
            .try_merge(&mut names_held)
            .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL rewriting"))?;
        drop(names_held);
        // each solution of the pattern's variables and blank nodes once, as matching the
        // stored and derived facts gives it, with the blank nodes then left out
        let mut gp = GraphPattern::Distinct {
            inner: Box::new(GraphPattern::Project {
                inner: Box::new(union),
                variables: names,
            }),
        };
        if visible_len < bgp.vars.len() {
            self.emitted_held
                .try_grow(visible_bytes)
                .map_err(|e| crate::query::memory_error(e, "the OWL 2 QL rewriting"))?;
            let mut visible = Vec::with_capacity(visible_len);
            visible.extend(bgp.vars.iter().filter_map(|(term, _)| match term {
                TermPattern::Variable(v) => Some(v.clone()),
                _ => None,
            }));
            gp = GraphPattern::Project {
                inner: Box::new(gp),
                variables: visible,
            };
        }
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
            G::Path { .. } => {
                if !self.path_warned {
                    self.path_warned = true;
                    self.warnings.push(
                        "a property path is not rewritten under OWL 2 QL, so the owl-rl \
                         demand program answered it, which may miss answers that need \
                         unnamed individuals"
                            .into(),
                    );
                }
                gp.clone()
            }
            G::Values { .. } => gp.clone(),
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
    bound: &[(String, Term)],
    tbox: &Tbox,
    max_union: usize,
    max_memory: Option<u64>,
    stop: &dyn Fn() -> Result<()>,
) -> Result<Option<Rewritten>> {
    rewrite_query_with_parent(query, bound, tbox, max_union, max_memory, None, stop)
}

/// [`rewrite_query`], reserving from `memory_parent` during construction. The returned
/// algebra is caller-owned. Use [`rewrite_query_guarded_with_parent`] to keep its credit.
pub fn rewrite_query_with_parent(
    query: &Query,
    bound: &[(String, Term)],
    tbox: &Tbox,
    max_union: usize,
    max_memory: Option<u64>,
    memory_parent: Option<crate::ParentHandle>,
    stop: &dyn Fn() -> Result<()>,
) -> Result<Option<Rewritten>> {
    Ok(rewrite_query_guarded_with_parent(
        query,
        bound,
        tbox,
        max_union,
        max_memory,
        memory_parent,
        stop,
    )?
    .map(GuardedRewritten::into_rewritten))
}

/// [`rewrite_query_with_parent`], returning the rewritten query paired with its memory
/// guard so emitted algebra stays charged until the caller drops it.
pub fn rewrite_query_guarded_with_parent(
    query: &Query,
    bound: &[(String, Term)],
    tbox: &Tbox,
    max_union: usize,
    max_memory: Option<u64>,
    memory_parent: Option<crate::ParentHandle>,
    stop: &dyn Fn() -> Result<()>,
) -> Result<Option<GuardedRewritten>> {
    let budget = crate::budget::Budget::with_parent(
        max_memory.map(|m| m.min(usize::MAX as u64) as usize),
        memory_parent,
    );
    rewrite_query_in_budget(query, bound, tbox, max_union, &budget, stop)
}

/// [`rewrite_query`], sharing an existing `budget` (so an active [`ChargedTbox`] and the
/// rewriting buffers charge the same local and parent limits concurrently).
pub(crate) fn rewrite_query_in_budget(
    query: &Query,
    bound: &[(String, Term)],
    tbox: &Tbox,
    max_union: usize,
    budget: &Arc<crate::budget::Budget>,
    stop: &dyn Fn() -> Result<()>,
) -> Result<Option<GuardedRewritten>> {
    use std::fmt;
    use std::mem::size_of;

    #[derive(Default)]
    struct SyntaxSize {
        text: usize,
        lists: usize,
        unbound: usize,
    }
    impl fmt::Write for SyntaxSize {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            self.text = self.text.checked_add(text.len()).ok_or(fmt::Error)?;
            self.lists += text.bytes().filter(|&b| b == b'(').count();
            self.unbound += usize::from(text == "UNDEF ");
            Ok(())
        }
    }

    if matches!(query, Query::Describe { .. }) {
        return Ok(None);
    }
    stop()?;
    let overflow = |_| sparkles_core::Error::invalid("OWL 2 QL algebra size overflow");
    let memory_error = |e| crate::query::memory_error(e, "the OWL 2 QL rewriting");
    let mut syntax = SyntaxSize::default();
    query.fmt_sse(&mut syntax).map_err(overflow)?;
    let mut sparql = SyntaxSize::default();
    fmt::write(&mut sparql, format_args!("{query}")).map_err(overflow)?;
    // Each SSE list accounts for the algebra node or container it describes. SPARQL
    // also prints VALUES cells whose unbound slots are absent from SSE.
    let algebra_bytes = syntax
        .text
        .saturating_add(syntax.lists.saturating_mul(2 * size_of::<GraphPattern>()))
        .saturating_add(
            sparql
                .unbound
                .saturating_mul(size_of::<Option<spargebra::term::GroundTerm>>()),
        )
        .saturating_add(512);
    let emitted_held = crate::budget::Held::try_new(budget, algebra_bytes).map_err(memory_error)?;
    let binding_bytes = bound.iter().fold(0usize, |bytes, (name, term)| {
        bytes
            .saturating_add(96 + name.len() * 2)
            .saturating_add(sparkles_core::explain::term_bytes(term))
    });
    let mut names_held =
        crate::budget::Held::try_new(budget, binding_bytes).map_err(memory_error)?;
    let (mut counts, all_distinguished) = {
        let _text_held = crate::budget::Held::try_new(budget, syntax.text).map_err(memory_error)?;
        let mut sse = String::with_capacity(syntax.text);
        query
            .fmt_sse_without_ask_projection(&mut sse)
            .map_err(overflow)?;
        let mut counts = FxHashMap::default();
        count_names(&sse, &mut counts, &mut names_held)?;
        (
            counts,
            sse.contains("(count)") || sse.contains("(count distinct)"),
        )
    };
    // Initial bindings make their variables visible outside their patterns.
    for (name, _) in bound {
        count_name(&format!("?{name}"), &mut counts, &mut names_held)?;
    }
    let mut rw = Rewriter {
        tbox,
        max_union,
        stop,
        bound: bound.iter().cloned().collect(),
        counts,
        all_distinguished,
        fresh: 0,
        path_warned: false,
        derive: Vec::new(),
        nodes: Vec::new(),
        warnings: tbox.warnings(),
        emitted_held,
        budget: budget.clone(),
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
    let memory_guard = (rw.emitted_held.bytes() > 0)
        .then(|| Arc::new(rw.emitted_held) as Arc<dyn std::any::Any + Send + Sync>);
    Ok(Some(GuardedRewritten::with_guard(
        Rewritten {
            query,
            derive: rw.derive,
            nodes: rw.nodes,
            warnings: rw.warnings,
        },
        memory_guard,
    )))
}

#[cfg(test)]
mod tests;
