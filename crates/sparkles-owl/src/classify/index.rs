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

use super::data::{DataVal, Ranges};
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
/// No expression, in the tables of expressions by class and by individual.
const NO_EX: Ex = Ex::MAX;

/// One interned class expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    Top,
    Bottom,
    /// a named class, by its index in [`Interner::classes`]
    Class(u32),
    /// `ObjectOneOf(a)`, by the individual's index
    Nominal(u32),
    Conj(Ex, Ex),
    Exist(Role, Ex),
    SelfLoop(Role),
    /// `DataSomeValuesFrom(d E)`, by the data role and the index of the data range in
    /// [`Interner::vals`]. A data value has no successors, so it needs no context: the
    /// index gives a positive restriction its consequences as told subsumers.
    DataSome(Role, u32),
    /// Only in the interner of a chunk of a parallel normalization: an intersection, by
    /// its index in [`Interner::ands`]. Its binary form depends on the order of the
    /// operands' ids, so it waits for the ids of the merged interner.
    And(u32),
    /// `ObjectAllValuesFrom(r F)` in a positive position, which only the Horn rules
    /// (roadmap item 603) read
    Forall(Role, Ex),
    /// `ObjectMaxCardinality(1 r F)` in a positive position, with F's negative form
    AtMost1(Role, Ex),
    /// a class that the Horn normalization introduces, which no axiom names
    Fresh(u32),
}

/// Entities named by an IRI, which a [`Names`] table keys by the IRI.
pub trait Named: Clone + Eq {
    fn name(&self) -> &str;
}

impl Named for Class {
    fn name(&self) -> &str {
        self.as_str()
    }
}

impl Named for NamedIndividual {
    fn name(&self) -> &str {
        self.as_str()
    }
}

/// The hash of an IRI that [`Names`] keys by.
#[inline]
pub fn name_hash(s: &str) -> u64 {
    use std::hash::BuildHasher;
    rustc_hash::FxBuildHasher.hash_one(s)
}

/// Named entities numbered in the order they are added, each held once, with the hash
/// of each IRI kept beside it, so that a table grows without hashing an IRI again and a
/// merge can look up an IRI that a chunk hashed.
#[derive(Clone, Debug)]
pub struct Names<T> {
    items: Vec<T>,
    hashes: Vec<u64>,
    table: hashbrown::HashTable<u32>,
    /// the bytes of the IRIs, each counted with a pointer and a length
    name_bytes: usize,
}

impl<T> Default for Names<T> {
    fn default() -> Names<T> {
        Names {
            items: Vec::new(),
            hashes: Vec::new(),
            table: hashbrown::HashTable::new(),
            name_bytes: 0,
        }
    }
}

impl<T> std::ops::Deref for Names<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.items
    }
}

impl<T: Named> Names<T> {
    pub fn get(&self, x: &T) -> Option<u32> {
        let h = name_hash(x.name());
        self.table
            .find(h, |&i| self.items[i as usize].name() == x.name())
            .copied()
    }

    /// The id of an entity, which is added when it is new.
    pub fn id(&mut self, x: &T) -> u32 {
        let h = name_hash(x.name());
        match self.find(h, x) {
            Ok(id) => id,
            Err(()) => self.push(h, x.clone()),
        }
    }

    /// The id of an entity whose hash the caller has, which moves in when it is new.
    #[inline]
    fn id_hashed(&mut self, h: u64, x: T) -> u32 {
        match self.find(h, &x) {
            Ok(id) => id,
            Err(()) => self.push(h, x),
        }
    }

    #[inline]
    fn find(&self, h: u64, x: &T) -> Result<u32, ()> {
        self.table
            .find(h, |&i| self.items[i as usize].name() == x.name())
            .copied()
            .ok_or(())
    }

    #[inline]
    fn push(&mut self, h: u64, x: T) -> u32 {
        let id = self.items.len() as u32;
        let hashes = &self.hashes;
        self.table.insert_unique(h, id, |&i| hashes[i as usize]);
        self.name_bytes += x.name().len() + 16;
        self.items.push(x);
        self.hashes.push(h);
        id
    }

    /// The bytes of the IRIs, each counted with a pointer and a length.
    pub fn name_bytes(&self) -> usize {
        self.name_bytes
    }

    /// The bytes the table takes beyond the entities, roughly.
    pub fn bytes(&self) -> usize {
        self.items.capacity() * std::mem::size_of::<T>()
            + self.hashes.capacity() * 8
            + self.table.capacity() * 5
    }

    /// Takes the entities and their hashes out, leaving the table empty.
    pub fn take(&mut self) -> (Vec<T>, Vec<u64>) {
        self.table.clear();
        self.name_bytes = 0;
        (
            std::mem::take(&mut self.items),
            std::mem::take(&mut self.hashes),
        )
    }

    /// Adds the names of chunks, each a list of distinct names with their hashes, in
    /// order, and answers each chunk's ids, which are those that adding the names one
    /// at a time would give. `poll` is called every [`POLL_MERGE`] names and stops the
    /// merge with its error.
    pub fn merge_chunks<E>(
        &mut self,
        chunks: Vec<(Vec<T>, Vec<u64>)>,
        poll: &mut dyn FnMut() -> Result<(), E>,
    ) -> Result<Vec<Vec<u32>>, E> {
        let n: usize = chunks.iter().map(|(xs, _)| xs.len()).sum();
        let hashes = &self.hashes;
        self.table.reserve(n, |&i| hashes[i as usize]);
        self.items.reserve(n);
        self.hashes.reserve(n);
        let mut done = 0usize;
        let mut out = Vec::with_capacity(chunks.len());
        for (xs, hs) in chunks {
            let mut ids = Vec::with_capacity(xs.len());
            for (x, h) in xs.into_iter().zip(hs) {
                done += 1;
                if done.is_multiple_of(POLL_MERGE) {
                    poll()?;
                }
                ids.push(self.id_hashed(h, x));
            }
            out.push(ids);
        }
        Ok(out)
    }
}

/// A chunk's expressions and roles by their ids in the merged interner.
pub type Mapped = (Vec<Ex>, Vec<Role>);

/// The names and expressions a merge of chunks takes between two calls of its poll.
pub const POLL_MERGE: usize = if cfg!(test) { 64 } else { 65_536 };

/// The name of a role.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RoleName {
    Object(ObjectProperty),
    Data(DataProperty),
    /// a role the binarization of a property chain introduces
    Fresh(u32),
    /// the inverse of an object property, which only a run with the Horn rules has
    Inverse(ObjectProperty),
}

/// The interned expressions and the names they use. Ids are never reused, so an
/// incremental run can keep contexts by id.
#[derive(Default)]
pub struct Interner {
    pub nodes: Vec<Node>,
    /// the compound expressions; a class's and an individual's are in
    /// [`Interner::class_nodes`] and [`Interner::individual_nodes`]
    map: FxHashMap<Node, Ex>,
    /// the expression of each class and individual by id, [`NO_EX`] before it has one
    class_nodes: Vec<Ex>,
    individual_nodes: Vec<Ex>,
    pub classes: Names<Class>,
    /// for each class, whether the run's normalization met it, which makes it a class of
    /// the signature
    pub met: Vec<bool>,
    pub individuals: Names<NamedIndividual>,
    /// the data ranges, as lattice elements
    pub vals: Vec<DataVal>,
    val_ids: FxHashMap<DataVal, u32>,
    /// the opaque data atoms and the datatype definitions of the run
    pub ranges: Ranges,
    pub roles: Vec<RoleName>,
    role_ids: FxHashMap<RoleName, Role>,
    fresh: u32,
    /// the fresh classes of the Horn normalization
    fresh_classes: u32,
    /// whether this is the interner of a chunk of a parallel normalization, which keeps
    /// intersections whole as [`Node::And`]
    chunk: bool,
    /// a chunk's intersections: the operands, and whether it is the binary intersection
    /// of [`Interner::conj`] rather than that of a class expression
    pub ands: Vec<(Vec<Ex>, bool)>,
    and_ids: FxHashMap<(Vec<Ex>, bool), u32>,
    /// the operands of [`Interner::ands`], counted twice, once for the table's key
    and_ops: usize,
}

impl Interner {
    pub fn new() -> Interner {
        let mut i = Interner::default();
        i.node(Node::Top);
        i.node(Node::Bottom);
        i
    }

    /// The interner of a chunk of a parallel normalization, with the datatype
    /// definitions of the run.
    pub fn chunk(definitions: &FxHashMap<Datatype, DataRange>) -> Interner {
        let mut i = Interner::new();
        i.chunk = true;
        i.ranges.definitions = definitions.clone();
        i
    }

    /// The bytes the interner takes, roughly: each expression with its entry in the
    /// table, the names with their IRIs, and a chunk's intersections.
    pub fn bytes(&self) -> usize {
        self.nodes.capacity() * 40
            + self.classes.bytes()
            + self.classes.name_bytes()
            + self.met.capacity()
            + self.class_nodes.capacity() * 4
            + self.individuals.bytes()
            + self.individuals.name_bytes()
            + self.individual_nodes.capacity() * 4
            + self.roles.capacity() * 48
            + self.ands.len() * 80
            + self.and_ops * 4
    }

    /// Whether a chunk met data ranges or literals, whose ids its merge does not map, so
    /// that the run normalizes on one thread instead.
    pub fn chunk_has_data(&self) -> bool {
        !self.vals.is_empty() || !self.ranges.opaque.is_empty()
    }

    /// An intersection kept whole in a chunk's interner.
    fn and(&mut self, ops: Vec<Ex>, binary: bool) -> Ex {
        let key = (ops, binary);
        let id = match self.and_ids.get(&key) {
            Some(&id) => id,
            None => {
                let id = self.ands.len() as u32;
                self.and_ops += 2 * key.0.len();
                self.ands.push(key.clone());
                self.and_ids.insert(key, id);
                id
            }
        };
        self.node(Node::And(id))
    }

    /// The binary form of the intersection of distinct expressions other than
    /// `owl:Thing` and `owl:Nothing`, nested in the order of their ids.
    fn fold_conj(&mut self, es: &mut [Ex]) -> Ex {
        es.sort_unstable();
        let mut acc = es[0];
        for &e in &es[1..] {
            acc = self.conj(acc, e);
        }
        acc
    }

    /// Takes in the interners of chunks, which normalized consecutive runs of axioms
    /// that follow those this interner has met, and answers the ids here of each
    /// chunk's expressions and roles. Each name and expression gets the id the chunk's
    /// first occurrence would have given it in a normalization on one thread: a chunk
    /// lists them in the order it met them, and those this interner has keep their ids.
    /// `poll` is told the bytes of this interner every [`POLL_MERGE`] names, roles,
    /// expressions and operands, counted over all the chunks, and stops the merge with
    /// its error.
    pub fn merge<E>(
        &mut self,
        mut chunks: Vec<Interner>,
        poll: &mut dyn FnMut(usize) -> Result<(), E>,
    ) -> Result<Vec<Mapped>, E> {
        debug_assert!(chunks.iter().all(|c| !c.chunk_has_data() && c.fresh == 0));
        let before = self.bytes();
        let classes = self.classes.merge_chunks(
            chunks.iter_mut().map(|c| c.classes.take()).collect(),
            &mut || poll(before),
        )?;
        for ids in &classes {
            for &id in ids {
                self.mark_met(id);
            }
        }
        let before = self.bytes();
        let individuals = self.individuals.merge_chunks(
            chunks.iter_mut().map(|c| c.individuals.take()).collect(),
            &mut || poll(before),
        )?;
        // the work since the last poll, over all the chunks
        let mut work = 0usize;
        let mut out = Vec::with_capacity(chunks.len());
        for ((chunk, classes), individuals) in chunks.into_iter().zip(classes).zip(individuals) {
            let mut roles: Vec<Role> = Vec::with_capacity(chunk.roles.len());
            for r in &chunk.roles {
                work += 1;
                if work >= POLL_MERGE {
                    work = 0;
                    poll(self.bytes())?;
                }
                roles.push(self.role(r.clone()));
            }
            self.map.reserve(chunk.map.len());
            let mut map: Vec<Ex> = Vec::with_capacity(chunk.nodes.len());
            let mut ops: Vec<Ex> = Vec::new();
            for n in &chunk.nodes {
                work += match *n {
                    Node::And(j) => 1 + chunk.ands[j as usize].0.len(),
                    _ => 1,
                };
                if work >= POLL_MERGE {
                    work = 0;
                    poll(self.bytes())?;
                }
                let e = match *n {
                    Node::Top => TOP,
                    Node::Bottom => BOTTOM,
                    Node::Class(k) => self.node(Node::Class(classes[k as usize])),
                    Node::Nominal(k) => self.node(Node::Nominal(individuals[k as usize])),
                    Node::Conj(a, b) => self.conj(map[a as usize], map[b as usize]),
                    Node::Exist(r, f) => self.node(Node::Exist(roles[r as usize], map[f as usize])),
                    Node::SelfLoop(r) => self.node(Node::SelfLoop(roles[r as usize])),
                    Node::DataSome(..) => unreachable!("a chunk with data merges on one thread"),
                    Node::Forall(..) | Node::AtMost1(..) | Node::Fresh(_) => {
                        unreachable!("the Horn normalization runs on the merged interner")
                    }
                    Node::And(j) => {
                        let (xs, binary) = &chunk.ands[j as usize];
                        ops.clear();
                        ops.extend(xs.iter().map(|&x| map[x as usize]));
                        // operands that the chunk kept apart can be one expression here,
                        // and the normalization on one thread takes each once
                        ops.sort_unstable();
                        ops.dedup();
                        match ops.len() {
                            1 => ops[0],
                            _ if *binary => self.conj(ops[0], ops[1]),
                            _ => self.fold_conj(&mut ops),
                        }
                    }
                };
                map.push(e);
            }
            out.push((map, roles));
        }
        poll(self.bytes())?;
        Ok(out)
    }

    /// The bytes that merging this chunk's interner can add to the merged one, roughly:
    /// its own and a binary intersection for each operand of its intersections, twice
    /// over, since the merged interner's lists and tables grow by doubling.
    pub fn merge_bytes(&self) -> usize {
        2 * self.bytes() + self.and_ops * 40
    }

    /// The id of a class, which the run's normalization meets.
    fn class_id(&mut self, c: &Class) -> u32 {
        let id = self.classes.id(c);
        self.mark_met(id);
        id
    }

    #[inline]
    fn mark_met(&mut self, id: u32) {
        if self.met.len() <= id as usize {
            self.met.resize(id as usize + 1, false);
        }
        self.met[id as usize] = true;
    }

    fn individual_id(&mut self, i: &NamedIndividual) -> u32 {
        self.individuals.id(i)
    }

    /// The role of a name, if the interner has it.
    pub fn role_id(&self, name: &RoleName) -> Option<Role> {
        self.role_ids.get(name).copied()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn node(&mut self, n: Node) -> Ex {
        let slot = match n {
            Node::Class(id) => Some((&mut self.class_nodes, id)),
            Node::Nominal(id) => Some((&mut self.individual_nodes, id)),
            _ => None,
        };
        if let Some((by_id, id)) = slot {
            let id = id as usize;
            if by_id.len() <= id {
                by_id.resize(id + 1, NO_EX);
            }
            if by_id[id] != NO_EX {
                return by_id[id];
            }
            let e = self.nodes.len() as Ex;
            by_id[id] = e;
            self.nodes.push(n);
            return e;
        }
        if let Some(&e) = self.map.get(&n) {
            return e;
        }
        let e = self.nodes.len() as Ex;
        self.nodes.push(n);
        self.map.insert(n, e);
        e
    }

    pub fn get(&self, n: &Node) -> Option<Ex> {
        let by_id = match *n {
            Node::Class(id) => Some((&self.class_nodes, id)),
            Node::Nominal(id) => Some((&self.individual_nodes, id)),
            _ => None,
        };
        match by_id {
            Some((v, id)) => v.get(id as usize).copied().filter(|&e| e != NO_EX),
            None => self.map.get(n).copied(),
        }
    }

    pub fn class(&mut self, c: &Class) -> Ex {
        match c.as_str() {
            vocab::OWL_THING => return TOP,
            vocab::OWL_NOTHING => return BOTTOM,
            _ => {}
        }
        let id = self.class_id(c);
        self.node(Node::Class(id))
    }

    /// The expression of a class, if the interner has it.
    pub fn class_ex(&self, c: &Class) -> Option<Ex> {
        match c.as_str() {
            vocab::OWL_THING => Some(TOP),
            vocab::OWL_NOTHING => Some(BOTTOM),
            _ => self
                .classes
                .get(c)
                .and_then(|id| self.get(&Node::Class(id))),
        }
    }

    /// Whether the run's normalization met the class `id`, which makes it a class of
    /// the signature.
    pub fn in_signature(&self, c: &Class) -> bool {
        self.classes.get(c).is_some_and(|id| self.met[id as usize])
    }

    /// Forgets which classes the normalization met, before a run normalizes the axioms.
    pub fn clear_met(&mut self) {
        self.met.iter_mut().for_each(|m| *m = false);
    }

    /// The expressions of the classes the run's normalization met, by class id.
    pub fn met_classes(&self) -> impl Iterator<Item = Ex> + '_ {
        self.met
            .iter()
            .enumerate()
            .filter(|(_, m)| **m)
            .filter_map(|(id, _)| self.get(&Node::Class(id as u32)))
    }

    pub fn nominal(&mut self, i: &NamedIndividual) -> Ex {
        let id = self.individual_id(i);
        self.node(Node::Nominal(id))
    }

    /// The id of a data range.
    pub fn val(&mut self, v: DataVal) -> u32 {
        match self.val_ids.get(&v) {
            Some(&id) => id,
            None => {
                let id = self.vals.len() as u32;
                self.vals.push(v.clone());
                self.val_ids.insert(v, id);
                id
            }
        }
    }

    /// The expressions of the individuals.
    pub fn nominals(&self) -> impl Iterator<Item = Ex> + '_ {
        (0..self.individuals.len() as u32).filter_map(|i| self.get(&Node::Nominal(i)))
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

    /// A class that no axiom names, for the Horn normalization.
    pub fn fresh_class(&mut self) -> Ex {
        self.fresh_classes += 1;
        self.node(Node::Fresh(self.fresh_classes))
    }

    /// The binary intersection of two expressions, with its operands ordered.
    pub fn conj(&mut self, a: Ex, b: Ex) -> Ex {
        if a == b {
            return a;
        }
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        if self.chunk {
            return self.and(vec![a, b], true);
        }
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
    /// for each data role, the intersection of its data ranges and those of its
    /// superroles, by index in [`Interner::vals`]
    pub data_range: Vec<Option<u32>>,
    /// for each data role, its functional superroles, itself included
    pub functional: Vec<Vec<Role>>,
    /// roles under the bottom object or data property, which relate nothing
    pub empty: Vec<bool>,
    /// whether any role is empty
    pub any_empty: bool,
    /// for each role, its inverse, [`NO_ROLE`] when the run has none
    pub inv: Vec<Role>,
    /// pairs of roles that cannot both link one element to another: a role under the
    /// first and a role under the second between the same pair is a clash, which
    /// disjoint and asymmetric roles give
    pub clash: Vec<(Role, Role)>,
}

/// No role, in [`Roles::inv`].
pub const NO_ROLE: Role = Role::MAX;

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
            + self.data_range.len() * 8
            + self
                .functional
                .iter()
                .map(|v| 24 + v.capacity() * 4)
                .sum::<usize>()
            + self.empty.len()
    }

    /// The functional superroles of a data role, itself included.
    #[inline]
    pub fn functional(&self, r: Role) -> &[Role] {
        self.functional
            .get(r as usize)
            .map_or(&[], |v| v.as_slice())
    }

    #[inline]
    pub fn is_empty_role(&self, r: Role) -> bool {
        self.any_empty && self.empty.get(r as usize).copied().unwrap_or(false)
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

    /// The inverse of a role, when the run has one.
    #[inline]
    pub fn inv(&self, r: Role) -> Option<Role> {
        self.inv.get(r as usize).copied().filter(|&i| i != NO_ROLE)
    }

    /// Whether two roles between the same pair of elements, both read from the first
    /// element to the second, contradict a disjointness or an asymmetry.
    pub fn clashes(&self, r1: Role, r2: Role) -> bool {
        self.clash
            .iter()
            .any(|&(s, t)| self.sub(r1, s) && self.sub(r2, t))
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

/// A key: the named instances of the class that share a named individual for every
/// object role and a data value for every data role are one individual (Structural
/// Specification §9.5, Direct Semantics Table 9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub class: Ex,
    pub objects: Vec<Role>,
    pub data: Vec<Role>,
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
    /// `range(d) = v`, for a data role and a data range
    pub data_ranges: Vec<(Role, u32)>,
    /// the functional data roles
    pub functional: Vec<Role>,
    /// `HasKey(c (p…) (d…))`
    pub keys: Vec<Key>,
    /// the individuals of assertions, which classification checks for consistency
    pub individuals: Vec<Ex>,
    /// the classes of the signature
    pub classes: Vec<Ex>,
    /// what the Horn normalization adds, when the run takes axioms outside OWL 2 EL
    pub horn: Option<Box<super::horn::HornAx>>,
}

impl Axioms {
    /// Appends a chunk's normalized axioms to these, with the chunk's ids of
    /// expressions and roles mapped to the merged interner's. The disjointness sets are
    /// sorted by id, as the normalization sorts them.
    pub fn append_mapped(&mut self, chunk: Axioms, nodes: &[Ex], roles: &[Role]) {
        let n = |e: Ex| nodes[e as usize];
        let r = |x: Role| roles[x as usize];
        debug_assert!(chunk.data_ranges.is_empty());
        self.subs
            .extend(chunk.subs.into_iter().map(|(a, b)| (n(a), n(b))));
        for mut d in chunk.disjoint {
            for e in &mut d {
                *e = n(*e);
            }
            d.sort_unstable();
            // two operands that the chunk kept apart and that are one expression here
            // make it unsatisfiable, as the normalization on one thread finds
            for w in d.windows(2) {
                if w[0] == w[1] {
                    self.subs.push((w[0], BOTTOM));
                }
            }
            d.dedup();
            if d.len() >= 2 {
                self.disjoint.push(d);
            }
        }
        self.role_subs
            .extend(chunk.role_subs.into_iter().map(|(a, b)| (r(a), r(b))));
        self.chains.extend(
            chunk
                .chains
                .into_iter()
                .map(|(rs, s)| (rs.into_iter().map(r).collect(), r(s))),
        );
        self.reflexive.extend(chunk.reflexive.into_iter().map(r));
        self.functional.extend(chunk.functional.into_iter().map(r));
        self.ranges
            .extend(chunk.ranges.into_iter().map(|(x, c)| (r(x), n(c))));
        self.keys.extend(chunk.keys.into_iter().map(|k| Key {
            class: n(k.class),
            objects: k.objects.into_iter().map(r).collect(),
            data: k.data.into_iter().map(r).collect(),
        }));
        self.individuals
            .extend(chunk.individuals.into_iter().map(n));
        self.classes.extend(chunk.classes.into_iter().map(n));
    }

    /// The bytes the normalized axioms take, roughly, from the lengths of the lists
    /// alone, so that a normalization can ask often.
    pub fn rough_bytes(&self) -> usize {
        self.subs.capacity() * 8
            + self.disjoint.capacity() * 40
            + self.role_subs.capacity() * 8
            + self.chains.capacity() * 48
            + self.reflexive.capacity() * 4
            + self.ranges.capacity() * 8
            + self.data_ranges.capacity() * 8
            + self.functional.capacity() * 4
            + self.keys.capacity() * 72
            + self.individuals.capacity() * 4
            + self.classes.capacity() * 4
    }

    /// The bytes the normalized axioms take.
    pub fn bytes(&self) -> usize {
        self.subs.capacity() * 8
            + self
                .disjoint
                .iter()
                .map(|d| 24 + d.capacity() * 4)
                .sum::<usize>()
            + self.disjoint.capacity() * 24
            + self.role_subs.capacity() * 8
            + self
                .chains
                .iter()
                .map(|(c, _)| 32 + c.capacity() * 4)
                .sum::<usize>()
            + self.reflexive.capacity() * 4
            + self.ranges.capacity() * 8
            + self.data_ranges.capacity() * 8
            + self.functional.capacity() * 4
            + self
                .keys
                .iter()
                .map(|k| 56 + (k.objects.len() + k.data.len()) * 4)
                .sum::<usize>()
            + self.individuals.capacity() * 4
            + self.classes.capacity() * 4
            + self.horn.as_ref().map_or(0, |h| h.bytes())
    }
}

/// Maps one axiom's logical content into the interner. `out` collects what it means,
/// and the answer says what of it the classifier may not fully take into account. An
/// axiom with an expression the classifier cannot represent adds nothing to `out`.
///
/// Every class of the axiom's signature is interned, and so met for the run: those the
/// normalization does not reach are interned from the signature.
pub fn normalize(ax: &AxiomKind, it: &mut Interner, out: &mut Axioms) -> Option<&'static str> {
    let (partial, whole) = match normalize_axiom(ax, it, out) {
        Ok((partial, whole)) => (partial, whole),
        Err(why) => (Some(why), false),
    };
    if !whole {
        intern_signature(ax, it);
    }
    partial
}

/// Interns the classes of an axiom's signature.
pub fn intern_signature(ax: &AxiomKind, it: &mut Interner) {
    let mut sig = crate::model::Signature::default();
    sig.axiom(ax);
    for e in &sig.0 {
        if let Entity::Class(c) = e {
            it.class(c);
        }
    }
}

/// [`normalize`], with an error for an axiom left out. Every branch interns all its
/// expressions before it adds to `out`, so that an error leaves `out` as it was. The
/// answer says, besides, whether the normalization interned every class of the axiom.
fn normalize_axiom(
    ax: &AxiomKind,
    it: &mut Interner,
    out: &mut Axioms,
) -> Result<(Option<&'static str>, bool), &'static str> {
    use AxiomKind as K;
    let mut partial: Option<&'static str> = None;
    let mut whole = true;
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
                    // every chain is a subproperty of the top property
                    let trivial = sup.named().as_str() == vocab::OWL_TOP_OBJECT_PROPERTY;
                    if !trivial
                        && ps
                            .iter()
                            .any(|p| p.named().as_str() == vocab::OWL_TOP_OBJECT_PROPERTY)
                    {
                        partial = Some(TOP_CHAIN);
                    }
                    match rs.len() {
                        0 => {}
                        1 => out.role_subs.push((rs[0], s)),
                        _ if trivial => {}
                        _ => out.chains.push((rs, s)),
                    }
                }
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
        }
        K::TransitiveObjectProperty(p) => {
            if let Some(r) = object_role(p, it)
                && p.named().as_str() != vocab::OWL_TOP_OBJECT_PROPERTY
            {
                out.chains.push((vec![r, r], r));
            }
        }
        K::ReflexiveObjectProperty(p) => {
            if let Some(r) = object_role(p, it) {
                out.reflexive.push(r);
            }
        }
        K::ObjectPropertyDomain(p, c) => {
            if let Some(r) = object_role(p, it) {
                let c = ce(c, it, &mut partial)?;
                let e = it.node(Node::Exist(r, TOP));
                out.subs.push((e, c));
            } else {
                whole = false;
            }
        }
        K::ObjectPropertyRange(p, c) => {
            if let Some(r) = object_role(p, it) {
                let c = ce(c, it, &mut partial)?;
                out.ranges.push((r, c));
            } else {
                whole = false;
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
            let top = it.val(DataVal::TOP);
            let e = it.node(Node::DataSome(r, top));
            out.subs.push((e, c));
        }
        K::DataPropertyRange(d, range) => {
            let r = it.role(RoleName::Data(d.clone()));
            let (v, opaque) = it.ranges.range(range);
            if opaque {
                partial = Some(OPAQUE);
            }
            let v = it.val(v);
            out.data_ranges.push((r, v));
        }
        K::FunctionalDataProperty(d) => {
            let r = it.role(RoleName::Data(d.clone()));
            out.functional.push(r);
        }
        // the definitions are read before the axioms, and the data ranges that name a
        // defined datatype stand for its definition
        K::DatatypeDefinition(_, range) => {
            if it.ranges.range(range).1 {
                partial = Some(OPAQUE);
            }
        }
        K::HasKey(c, ops, dps) => {
            let class = ce(c, it, &mut partial)?;
            let mut objects = Vec::with_capacity(ops.len());
            for p in ops {
                let Some(r) = object_role(p, it) else {
                    return Err("inverse properties");
                };
                objects.push(r);
            }
            let data = dps
                .iter()
                .map(|d| it.role(RoleName::Data(d.clone())))
                .collect();
            out.keys.push(Key {
                class,
                objects,
                data,
            });
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
        }
        // ¬p(a, b) is {a} ⊓ ∃p.{b} ⊑ ⊥
        K::NegativeObjectPropertyAssertion(p, Individual::Named(a), Individual::Named(b)) => {
            let Some(r) = object_role(p, it) else {
                return Err("inverse properties");
            };
            let a = it.nominal(a);
            let b = it.nominal(b);
            let e = it.node(Node::Exist(r, b));
            let both = it.conj(a, e);
            out.individuals.push(a);
            out.individuals.push(b);
            out.subs.push((both, BOTTOM));
        }
        K::DataPropertyAssertion(d, Individual::Named(a), v) => {
            let r = it.role(RoleName::Data(d.clone()));
            let a = it.nominal(a);
            let (v, opaque) = it.ranges.literal(v);
            if opaque {
                partial = Some(OPAQUE);
            }
            let v = it.val(v);
            let e = it.node(Node::DataSome(r, v));
            out.individuals.push(a);
            out.subs.push((a, e));
        }
        K::NegativeDataPropertyAssertion(d, Individual::Named(a), v) => {
            let r = it.role(RoleName::Data(d.clone()));
            let a = it.nominal(a);
            let (v, opaque) = it.ranges.literal(v);
            if opaque {
                partial = Some(OPAQUE);
            }
            let v = it.val(v);
            let e = it.node(Node::DataSome(r, v));
            let both = it.conj(a, e);
            out.individuals.push(a);
            out.subs.push((both, BOTTOM));
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
        // annotations carry no logical content
        K::AnnotationAssertion(..)
        | K::SubAnnotationPropertyOf(..)
        | K::AnnotationPropertyDomain(..)
        | K::AnnotationPropertyRange(..) => {}
        // the profile check leaves the remaining types out of OWL 2 EL, anonymous
        // individuals among them
        _ => whole = false,
    }
    Ok((partial, whole))
}

/// Why an axiom with a chain through the top object property is used in part: the
/// classifier composes links, and the top property links every pair.
const TOP_CHAIN: &str = "property chains through the top object property";
/// Why an axiom with a literal or a datatype that the classifier cannot compare is used
/// in part: it is an atom that only equals itself.
pub const OPAQUE: &str = "literals and datatypes without values the classifier compares";

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
                .collect::<Result<_, &'static str>>()?;
            es.sort_unstable();
            es.dedup();
            es.retain(|&e| e != TOP);
            if es.contains(&BOTTOM) {
                return Ok((BOTTOM, partial));
            }
            match es.len() {
                0 => (TOP, partial),
                1 => (es[0], partial),
                _ if it.chunk => (it.and(es, false), partial),
                _ => (it.fold_conj(&mut es), partial),
            }
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
                    (it.node(Node::Exist(r, f)), None)
                }
                ObjectPropertyExpression::InverseOf(_) => return Err("inverse properties"),
            }
        }
        C::ObjectOneOf(is) if is.len() == 1 => match is.iter().next() {
            Some(Individual::Named(i)) => (it.nominal(i), None),
            _ => return Err("anonymous individuals"),
        },
        C::ObjectHasSelf(ObjectPropertyExpression::Property(p)) => {
            let r = it.role(RoleName::Object(p.clone()));
            (it.node(Node::SelfLoop(r)), None)
        }
        C::DataSomeValuesFrom(ps, r) if ps.len() == 1 => {
            let role = it.role(RoleName::Data(ps[0].clone()));
            let (v, opaque) = it.ranges.range(r);
            let v = it.val(v);
            (it.node(Node::DataSome(role, v)), opaque.then_some(OPAQUE))
        }
        C::DataHasValue(d, l) => {
            let role = it.role(RoleName::Data(d.clone()));
            let (v, opaque) = it.ranges.literal(l);
            let v = it.val(v);
            (it.node(Node::DataSome(role, v)), opaque.then_some(OPAQUE))
        }
        // outside OWL 2 EL: the profile check has already left the axiom out
        _ => return Err("a class expression outside OWL 2 EL"),
    })
}

/// A slice of one of the index's flat arrays.
#[derive(Clone, Copy, Default, Debug)]
struct Span {
    start: u32,
    len: u32,
}

impl Span {
    #[inline]
    fn of<T>(self, v: &[T]) -> &[T] {
        &v[self.start as usize..(self.start + self.len) as usize]
    }
}

/// What a context needs when it derives an expression, in one record so that a new
/// subsumer costs one lookup: the expression, the context of its filler when it is an
/// existential restriction, and the spans of its rules in the flat arrays.
#[derive(Clone, Copy, Debug)]
pub struct Head {
    pub node: Node,
    /// for an existential restriction, the context of its filler, which includes the
    /// role's range when the restriction occurs positively; for a data restriction, the
    /// index in [`Index::vals`] of its range met with the role's data range
    pub succ: Ex,
    told: Span,
    conj: Span,
    exist: Span,
    disj: Span,
}

/// The rules each expression triggers.
#[derive(Default)]
pub struct Index {
    /// by expression
    heads: Vec<Head>,
    /// told superexpressions
    told: Vec<Ex>,
    /// for an operand, its partner and the negative intersection they form
    conj: Vec<(Ex, Ex)>,
    /// for a filler, the role and the negative existential restriction
    exist: Vec<(Role, Ex)>,
    /// for a member, the disjointness sets it is in
    disjoint: Vec<u32>,
    /// negative `ObjectHasSelf` restrictions: the role and the expression
    pub selfs: Vec<(Role, Ex)>,
    pub roles: Roles,
    /// the data ranges, by index
    pub vals: Vec<DataVal>,
    /// the negative data restrictions by role: the range and the expression
    pub neg_data: FxHashMap<Role, Vec<(u32, Ex)>>,
    /// the negative existential restrictions on a universal role, which hold everywhere
    /// once their filler has an instance: the filler and the expression
    pub universal: Vec<(Ex, Ex)>,
    pub keys: Vec<Key>,
    /// the expressions of the individuals
    pub nominals: Vec<Ex>,
    /// whether a link to an individual can close a loop that a negative `ObjectHasSelf`
    /// reads
    pub nominal_selfs: bool,
    /// the top object and data roles, when the axioms use them
    pub tops: [Option<Role>; 2],
    /// what the Horn rules read, when the run takes axioms outside OWL 2 EL
    pub horn: Option<Box<super::horn::HornIx>>,
}

impl Index {
    /// Whether `r` is above a top role, so that it relates every pair of elements.
    pub fn is_universal(&self, r: Role) -> bool {
        self.tops.iter().flatten().any(|&t| self.roles.sub(t, r))
    }
}

const NO_HEAD: Head = Head {
    node: Node::Top,
    succ: TOP,
    told: Span { start: 0, len: 0 },
    conj: Span { start: 0, len: 0 },
    exist: Span { start: 0, len: 0 },
    disj: Span { start: 0, len: 0 },
};

/// Groups `(key, value)` pairs by key into one flat array, in the order of the pairs
/// within a key, and sets each key's span.
fn group<T: Copy>(
    pairs: &[(Ex, T)],
    span: impl Fn(&mut Head) -> &mut Span,
    heads: &mut [Head],
) -> Vec<T> {
    let Some(&(_, fill)) = pairs.first() else {
        return Vec::new();
    };
    let mut next = vec![0u32; heads.len()];
    for &(k, _) in pairs {
        next[k as usize] += 1;
    }
    let mut at = 0u32;
    for (k, c) in next.iter_mut().enumerate() {
        *span(&mut heads[k]) = Span { start: at, len: *c };
        let start = at;
        at += *c;
        *c = start;
    }
    let mut out = vec![fill; pairs.len()];
    for &(k, v) in pairs {
        out[next[k as usize] as usize] = v;
        next[k as usize] += 1;
    }
    out
}

impl Index {
    #[inline]
    pub fn head(&self, e: Ex) -> &Head {
        self.heads.get(e as usize).unwrap_or(&NO_HEAD)
    }
    #[inline]
    pub fn told_of(&self, h: &Head) -> &[Ex] {
        h.told.of(&self.told)
    }
    #[inline]
    pub fn conj_of(&self, h: &Head) -> &[(Ex, Ex)] {
        h.conj.of(&self.conj)
    }
    #[inline]
    pub fn exist_of(&self, h: &Head) -> &[(Role, Ex)] {
        h.exist.of(&self.exist)
    }
    #[inline]
    pub fn disjoint_of(&self, h: &Head) -> &[u32] {
        h.disj.of(&self.disjoint)
    }
    pub fn exist(&self, e: Ex) -> &[(Role, Ex)] {
        self.exist_of(self.head(e))
    }
    pub fn disjoint(&self, e: Ex) -> &[u32] {
        self.disjoint_of(self.head(e))
    }

    /// Whether the index has individuals or universal roles, which the run's later steps
    /// look at: the merges of contexts with individuals, the conditional worlds and the
    /// keys.
    pub fn has_individuals(&self) -> bool {
        !self.nominals.is_empty() || !self.universal.is_empty()
    }

    /// The negative data restrictions that a data value in `v` by a role under `r` meets.
    pub fn data_matches(&self, r: Role, v: &DataVal, mut f: impl FnMut(Ex)) {
        for &g in self
            .roles
            .sup
            .get(r as usize)
            .map_or(&[][..], |v| v.as_slice())
        {
            if let Some(ns) = self.neg_data.get(&g) {
                for &(w, x) in ns {
                    if v.within(&self.vals[w as usize]) {
                        f(x);
                    }
                }
            }
        }
    }

    /// The bytes the index takes, roughly, the role table included.
    pub fn bytes(&self) -> usize {
        self.heads.capacity() * std::mem::size_of::<Head>()
            + self.told.capacity() * 4
            + self.conj.capacity() * 8
            + self.exist.capacity() * 8
            + self.disjoint.capacity() * 4
            + self.selfs.capacity() * 8
            + self.vals.capacity() * std::mem::size_of::<DataVal>()
            + self
                .neg_data
                .values()
                .map(|v| 48 + v.capacity() * 8)
                .sum::<usize>()
            + self.universal.capacity() * 8
            + self.nominals.capacity() * 4
            + self.roles.bytes()
            + self.horn.as_ref().map_or(0, |h| h.bytes())
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
        let mut b = Builder::default();
        let mut told: Vec<(Ex, Ex)> = Vec::with_capacity(ax.subs.len());
        // the negative occurrences first, so that the data restrictions that occur
        // positively find every negative one they meet
        for &(a, _) in &ax.subs {
            b.negative(a, it);
        }
        let mut disj: Vec<(Ex, u32)> = Vec::new();
        for (s, set) in ax.disjoint.iter().enumerate() {
            for &e in set {
                b.negative(e, it);
                disj.push((e, s as u32));
            }
        }
        for k in &ax.keys {
            b.negative(k.class, it);
        }
        if let Some(h) = &ax.horn {
            for &e in &h.premises {
                b.negative(e, it);
            }
        }
        for &(r, w, x) in &b.neg_data {
            ix.neg_data.entry(r).or_default().push((w, x));
        }
        ix.vals = it.vals.clone();
        for &(a, c) in &ax.subs {
            b.positive(c, it, &mut ix);
            if a != c {
                told.push((a, c));
            }
        }
        // a reflexive role links every individual to itself, and a universal role every
        // individual to every one, so their ranges hold everywhere
        let mut everywhere: Vec<Ex> = ix
            .roles
            .reflexive
            .iter()
            .filter_map(|&r| ix.roles.range[r as usize])
            .collect();
        let top = it.role_id(&RoleName::Object(ObjectProperty::top()));
        if let Some(t) = top
            && let Some(c) = ix.roles.range[t as usize]
        {
            everywhere.push(c);
        }
        for c in everywhere {
            b.positive(c, it, &mut ix);
            told.push((TOP, c));
        }
        // the domain is never empty, so a universal role under the bottom role relates
        // what it cannot
        let top_data = it.role_id(&RoleName::Data(DataProperty::top()));
        if top
            .iter()
            .chain(&top_data)
            .any(|&t| ix.roles.is_empty_role(t))
        {
            told.push((TOP, BOTTOM));
        }
        ix.tops = [top, top_data];
        // every element is its own successor by a universal role
        for &(r, x) in &b.selfs {
            if top.is_some_and(|t| ix.roles.sub(t, r)) {
                told.push((TOP, x));
            }
        }
        // the negative restrictions on a universal role
        for &(f, (r, x)) in &b.exist {
            if top.is_some_and(|t| ix.roles.sub(t, r)) {
                ix.universal.push((f, x));
            }
        }
        ix.universal.sort_unstable();
        ix.universal.dedup();
        // the data value of a data restriction is in its range and the role's
        let datas: Vec<(Ex, Role, u32)> = it
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(e, n)| match *n {
                Node::DataSome(r, w) => Some((e as Ex, r, w)),
                _ => None,
            })
            .collect();
        let mut data_succ = Vec::with_capacity(datas.len());
        for (e, r, w) in datas {
            let mut v = it.vals[w as usize].clone();
            if let Some(d) = ix.roles.data_range.get(r as usize).copied().flatten() {
                v = v.meet(&it.vals[d as usize]);
            }
            data_succ.push((e, it.val(v)));
        }
        // every individual is linked to every literal by a universal data role
        if let Some(td) = top_data {
            for (&g, ns) in &ix.neg_data {
                if ix.roles.sub(td, g) {
                    for &(w, x) in ns {
                        if !ix.vals[w as usize].is_empty() {
                            told.push((TOP, x));
                        }
                    }
                }
            }
            // every literal is in the data ranges of a universal data role, and none has
            // one value by it
            if let Some(w) = ix.roles.data_range[td as usize] {
                let v = &ix.vals[w as usize];
                if v.known != super::data::Known::Top {
                    told.push((TOP, BOTTOM));
                }
            }
            if !ix.roles.functional(td).is_empty() {
                told.push((TOP, BOTTOM));
            }
        }
        if let Some(h) = &ax.horn {
            // the run takes the Horn rules only when one can fire: without universals,
            // at-most restrictions, loops or role clashes the EL rules derive all
            let hi = horn_index(ax, h, it, &mut ix, &mut b, &mut told);
            let fires = !ix.roles.clash.is_empty()
                || !ix.roles.reflexive.is_empty()
                || it
                    .nodes
                    .iter()
                    .any(|n| matches!(n, Node::Forall(..) | Node::AtMost1(..) | Node::SelfLoop(_)));
            if fires {
                ix.horn = Some(Box::new(hi));
            }
        }
        ix.heads = it
            .nodes
            .iter()
            .map(|&node| Head {
                node,
                succ: match node {
                    Node::Exist(_, f) => f,
                    _ => TOP,
                },
                ..NO_HEAD
            })
            .collect();
        for &(e, succ) in b.succ.iter().chain(&data_succ) {
            ix.heads[e as usize].succ = succ;
        }
        told.extend(b.told.iter().copied());
        // told superexpressions, sorted and without duplicates
        told.sort_unstable();
        told.dedup();
        ix.told = group(&told, |h| &mut h.told, &mut ix.heads);
        ix.conj = group(&b.conj, |h| &mut h.conj, &mut ix.heads);
        ix.exist = group(&b.exist, |h| &mut h.exist, &mut ix.heads);
        ix.disjoint = group(&disj, |h| &mut h.disj, &mut ix.heads);
        ix.nominal_selfs = !b.selfs.is_empty() && !it.individuals.is_empty();
        ix.selfs = b.selfs;
        ix.keys = ax.keys.clone();
        ix.nominals = it.nominals().collect();
        ix.vals = it.vals.clone();
        Ok(ix)
    }
}

/// The index of the Horn rules, built after the occurrences of the normalized axioms:
/// the universals that the negative existential restrictions stand for where links run
/// against them, the negative existentials that the ranges stand for, the automata of the
/// universals on roles that chains reach, and the fillers of the at-most restrictions.
fn horn_index(
    ax: &Axioms,
    h: &super::horn::HornAx,
    it: &mut Interner,
    ix: &mut Index,
    b: &mut Builder,
    told: &mut Vec<(Ex, Ex)>,
) -> super::horn::HornIx {
    let roles = &ix.roles;
    // the roles of the links the saturation creates: those of the positive existential
    // restrictions, and the inverses of those that an at-most restriction on an inverse
    // role can identify a predecessor through
    let mut links: Vec<Role> = b
        .succ
        .iter()
        .filter_map(|&(e, _)| match it.nodes[e as usize] {
            Node::Exist(r, _) => Some(r),
            _ => None,
        })
        .collect();
    links.sort_unstable();
    links.dedup();
    let most: Vec<Role> = it
        .nodes
        .iter()
        .filter_map(|n| match *n {
            Node::AtMost1(s, _) => Some(s),
            _ => None,
        })
        .collect();
    let back: Vec<Role> = links
        .iter()
        .filter(|&&l| most.iter().any(|&s| roles.sub(l, s)))
        .filter_map(|&l| roles.inv(l))
        .collect();
    links.extend(back);
    links.sort_unstable();
    links.dedup();
    let linked = |s: Role| links.iter().any(|&l| roles.sub(l, s));
    // a range of s holds of the predecessors of the links that run against s
    let mut news: Vec<(Ex, Ex)> = Vec::new();
    for &(s, c) in &ax.ranges {
        if let Some(i) = roles.inv(s)
            && linked(i)
        {
            let e = it.node(Node::Exist(i, TOP));
            news.push((e, c));
        }
    }
    for &(e, c) in &news {
        b.negative(e, it);
        told.push((e, c));
    }
    // ∃r.F ⊑ G is F ⊑ ∀r⁻.G, which the backward links propagate as for EL, and which the
    // successors of F's contexts by roles under r⁻ need too. A filler ⊤ is a domain, which
    // the role's inverse takes as a range.
    // a universal on a role with an automaton is unfolded on its holder: on F itself for
    // those of the negative existentials, and on the universal for the others
    let mut unfold: Vec<(Ex, Role, Ex)> = Vec::new();
    let mut alls: Vec<(Ex, Ex)> = Vec::new();
    for &(f, (r, e)) in &b.exist {
        if f == TOP {
            continue;
        }
        let Some(i) = roles.inv(r) else { continue };
        match h.autos.get(&i) {
            Some(a) if a.trans.iter().any(|t| t.1.is_some_and(linked)) => unfold.push((f, i, e)),
            Some(_) => {}
            None if linked(i) => {
                let fa = it.node(Node::Forall(i, e));
                alls.push((f, fa));
            }
            None => {}
        }
    }
    for (f, fa) in alls {
        told.push((f, fa));
        b.positive(fa, it, ix);
    }
    for &(_, _, e) in &unfold {
        b.positive(e, it, ix);
    }
    // the universals on roles that chains reach, unfolded by the automata, each state a
    // fresh class; the universals the automata make are not unfolded again
    let mut flags = vec![0u8; it.nodes.len()];
    for (e, n) in it.nodes.iter().enumerate() {
        if let Node::Forall(r, f) = *n
            && h.autos.contains_key(&r)
        {
            flags[e] |= super::horn::HornIx::UNFOLDED;
            unfold.push((e as Ex, r, f));
        }
    }
    // each state of an automaton without ε-transitions is a class: the holder for the
    // start when no transition enters it, the filler for a final state that none leaves,
    // and a fresh class otherwise
    let mut autos: FxHashMap<Role, super::horn::EpsFree> = FxHashMap::default();
    // the universals the transitions make, which the rules apply even when one is a
    // universal that is unfolded too
    let mut made: Vec<Ex> = Vec::new();
    for (holder, r, f) in unfold {
        let a = autos.entry(r).or_insert_with(|| h.autos[&r].eps_free());
        let states: Vec<Ex> = (0..a.states)
            .map(|s| {
                if s == a.start && !a.entered[s as usize] {
                    holder
                } else if a.finals[s as usize] && !a.left[s as usize] {
                    f
                } else {
                    it.fresh_class()
                }
            })
            .collect();
        let start = states[a.start as usize];
        if start != holder {
            told.push((holder, start));
        }
        for (s, &x) in states.iter().enumerate() {
            if a.finals[s] && x != f {
                told.push((x, f));
            }
        }
        for &(p, l, q) in &a.trans {
            let x = it.node(Node::Forall(l, states[q as usize]));
            made.push(x);
            if states[p as usize] != x {
                told.push((states[p as usize], x));
            }
        }
    }
    flags.resize(it.nodes.len(), 0);
    for x in made {
        flags[x as usize] &= !super::horn::HornIx::UNFOLDED;
    }
    for n in &it.nodes {
        if let Node::AtMost1(_, f) = *n {
            flags[f as usize] |= super::horn::HornIx::FILLER;
        }
    }
    super::horn::HornIx {
        flags,
        base: it.nodes.len() as Ex,
    }
}

/// A set of expressions, as a flag for each.
#[derive(Default)]
struct Seen(Vec<bool>);

impl Seen {
    /// Adds an expression, and says whether it is new.
    #[inline]
    fn insert(&mut self, e: Ex) -> bool {
        let i = e as usize;
        if self.0.len() <= i {
            self.0.resize((i + 1).max(2 * self.0.len()), false);
        }
        !std::mem::replace(&mut self.0[i], true)
    }
}

/// The occurrences an index build collects before it groups them by expression.
#[derive(Default)]
struct Builder {
    neg_seen: Seen,
    pos_seen: Seen,
    conj: Vec<(Ex, (Ex, Ex))>,
    exist: Vec<(Ex, (Role, Ex))>,
    selfs: Vec<(Role, Ex)>,
    /// negative data restrictions: the role, the range and the expression
    neg_data: Vec<(Role, u32, Ex)>,
    succ: Vec<(Ex, Ex)>,
    /// the told subsumers of positive data restrictions and empty roles
    told: Vec<(Ex, Ex)>,
}

impl Builder {
    /// Registers a negative occurrence: the rules that build the expression from its
    /// parts.
    fn negative(&mut self, e: Ex, it: &Interner) {
        let mut stack = vec![e];
        while let Some(e) = stack.pop() {
            if !self.neg_seen.insert(e) {
                continue;
            }
            match it.nodes[e as usize] {
                Node::Conj(a, b) => {
                    self.conj.push((a, (b, e)));
                    self.conj.push((b, (a, e)));
                    stack.push(b);
                    stack.push(a);
                }
                Node::Exist(r, f) => {
                    self.exist.push((f, (r, e)));
                    stack.push(f);
                }
                Node::SelfLoop(r) => self.selfs.push((r, e)),
                Node::DataSome(r, v) => self.neg_data.push((r, v, e)),
                _ => {}
            }
        }
    }

    /// Registers a positive occurrence: the successor context of each existential
    /// restriction in it, and the consequences of each data restriction.
    fn positive(&mut self, e: Ex, it: &mut Interner, ix: &mut Index) {
        if !self.pos_seen.insert(e) {
            return;
        }
        match it.nodes[e as usize] {
            Node::Conj(a, b) => {
                self.positive(a, it, ix);
                self.positive(b, it, ix);
            }
            Node::Exist(r, f) => {
                self.positive(f, it, ix);
                if ix.roles.is_empty_role(r) {
                    self.told.push((e, BOTTOM));
                }
                let succ = match ix.roles.range.get(r as usize).copied().flatten() {
                    Some(range) => {
                        self.positive(range, it, ix);
                        it.conj(f, range)
                    }
                    None => f,
                };
                self.succ.push((e, succ));
            }
            Node::SelfLoop(r) => {
                if ix.roles.is_empty_role(r) {
                    self.told.push((e, BOTTOM));
                }
            }
            Node::Forall(_, f) => self.positive(f, it, ix),
            Node::DataSome(r, w) => {
                // the one data value of the restriction is in its range and the role's
                let mut v = it.vals[w as usize].clone();
                if let Some(d) = ix.roles.data_range.get(r as usize).copied().flatten() {
                    v = v.meet(&it.vals[d as usize]);
                }
                if v.is_empty() || ix.roles.is_empty_role(r) {
                    self.told.push((e, BOTTOM));
                } else {
                    let mut found = Vec::new();
                    ix.data_matches(r, &v, |x| found.push(x));
                    for x in found {
                        if x != e {
                            self.told.push((e, x));
                        }
                    }
                }
            }
            _ => {}
        }
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
pub(super) const POLL_WORK: usize = 65_536;

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
    // with the Horn rules, a chain holds in its mirror image too, r1⁻ ∘ … ∘ rn⁻ ⊑ s⁻ for
    // r1 ∘ … ∘ rn ⊑ s reversed, which composes the links a told chain over inverses gives
    let mirrors: Vec<(Vec<Role>, Role)> = match &ax.horn {
        Some(_) => {
            let told: FxHashSet<&(Vec<Role>, Role)> = ax.chains.iter().collect();
            let inv = super::horn::inverses(it);
            let inv = |r: Role| Some(inv[r as usize]).filter(|&i| i != NO_ROLE);
            let mut m: Vec<(Vec<Role>, Role)> = ax
                .chains
                .iter()
                .filter_map(|(rs, s)| {
                    let m: Option<Vec<Role>> = rs.iter().rev().map(|&r| inv(r)).collect();
                    Some((m?, inv(*s)?))
                })
                .filter(|c| !told.contains(c))
                .collect();
            m.sort();
            m.dedup();
            m
        }
        None => Vec::new(),
    };
    for (rs, s) in ax.chains.iter().chain(&mirrors) {
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
    // every object property is a subproperty of the top object property, and every
    // data property of the top data property
    if let Some(&top) = it.role_ids.get(&RoleName::Object(ObjectProperty::top())) {
        for (r, name) in it.roles.iter().enumerate() {
            if matches!(name, RoleName::Object(_)) && r as Role != top {
                direct[r].push(top);
            }
        }
    }
    if let Some(&top) = it.role_ids.get(&RoleName::Data(DataProperty::top())) {
        for (r, name) in it.roles.iter().enumerate() {
            if matches!(name, RoleName::Data(_)) && r as Role != top {
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
    let mut direct_data: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &(r, v) in &ax.data_ranges {
        direct_data[r as usize].push(v);
    }
    let mut data_range = vec![None; n];
    let mut is_functional = vec![false; n];
    for &r in &ax.functional {
        is_functional[r as usize] = true;
    }
    let mut functional: Vec<Vec<Role>> = vec![Vec::new(); n];
    for r in 0..n {
        if !matches!(it.roles[r], RoleName::Data(_)) {
            continue;
        }
        tick(1 + sup[r].len())?;
        let mut acc: Option<DataVal> = None;
        for &s in &sup[r] {
            for &w in &direct_data[s as usize] {
                let v = &it.vals[w as usize];
                acc = Some(match acc {
                    None => v.clone(),
                    Some(a) => a.meet(v),
                });
            }
            if is_functional[s as usize] {
                functional[r].push(s);
            }
        }
        if let Some(v) = acc {
            data_range[r] = Some(it.val(v));
        }
    }
    // a role under the bottom property relates nothing
    let mut empty = vec![false; n];
    for bottom in [
        RoleName::Object(ObjectProperty::bottom()),
        RoleName::Data(DataProperty::bottom()),
    ] {
        if let Some(&b) = it.role_ids.get(&bottom) {
            for (r, ss) in sup.iter().enumerate() {
                if ss.binary_search(&b).is_ok() {
                    empty[r] = true;
                }
            }
        }
    }
    // with the Horn rules: the inverses, and the roles that clash with themselves, which
    // relate nothing
    let (inv, mut clash) = match &ax.horn {
        Some(h) => (super::horn::inverses(it), h.clash.clone()),
        None => (Vec::new(), Vec::new()),
    };
    // two roles that clash clash on their inverses too
    let mirrored: Vec<(Role, Role)> = clash
        .iter()
        .filter_map(|&(s, t)| Some((*inv.get(s as usize)?, *inv.get(t as usize)?)))
        .filter(|&(s, t)| s != NO_ROLE && t != NO_ROLE)
        .collect();
    clash.extend(mirrored);
    clash.sort_unstable();
    clash.dedup();
    let mut roles = Roles {
        sup,
        comp,
        right,
        left,
        reflexive: reflexive.into_iter().collect(),
        range,
        data_range,
        functional,
        any_empty: false,
        empty,
        inv,
        clash,
    };
    if !roles.clash.is_empty() {
        for r in 0..n as Role {
            if roles.clashes(r, r) {
                roles.empty[r as usize] = true;
            }
        }
    }
    roles.any_empty = roles.empty.contains(&true);
    Ok(roles)
}
