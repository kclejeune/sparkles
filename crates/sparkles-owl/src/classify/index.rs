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
use super::dtcheck::atoms::{DtKey, DtRun, DtTable};
use super::dtcheck::range::{Ranges as DtRanges, Rid};
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
    /// a data restriction over a property of a checker component, by its index in the
    /// atoms of [`Interner::dt`] (F24, "The datatype checker")
    DtAtom(u32),
    /// a told subsumer of the checker's body atoms, by its expression, which the index
    /// tells the subsumer so that a context re-evaluates the atoms' components when it
    /// gains the subsumer
    DtFire(Ex),
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

    /// Logical input-size measure of the table, independent of allocator capacities.
    pub fn logical_bytes(&self) -> usize {
        self.items.len() * (std::mem::size_of::<T>() + 8 + 5)
    }

    /// Exact retained heap bytes of this table, including the hashbrown bucket allocation.
    pub fn exact_bytes(&self) -> usize {
        self.items
            .capacity()
            .saturating_mul(std::mem::size_of::<T>())
            .saturating_add(
                self.hashes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<u64>()),
            )
            .saturating_add(super::dl_clause::hash_table_bytes(
                self.table.capacity(),
                std::mem::size_of::<u32>(),
            ))
            .saturating_add(self.name_bytes)
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

/// The hash of a role's name that [`Interner`] keys by, which [`object_hash`] gives an
/// object property from its IRI alone.
fn role_hash(r: &RoleName) -> u64 {
    use std::hash::BuildHasher;
    let fx = rustc_hash::FxBuildHasher;
    match r {
        RoleName::Object(p) => object_hash(p.as_str()),
        RoleName::Data(p) => fx.hash_one((1u8, p.as_str())),
        RoleName::Fresh(n) => fx.hash_one((2u8, *n)),
        RoleName::Inverse(p) => fx.hash_one((3u8, p.as_str())),
    }
}

#[inline]
fn object_hash(iri: &str) -> u64 {
    use std::hash::BuildHasher;
    rustc_hash::FxBuildHasher.hash_one((0u8, iri))
}

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
    /// Actual opaque-array capacities owned by both vals and the map's keys.
    val_opaque_bytes: usize,
    /// the opaque data atoms and the datatype definitions of the run
    pub ranges: Ranges,
    /// the checker's data atoms, kept across runs like [`Interner::vals`]
    pub dt: DtTable,
    /// the run's checker components and definitions, while it normalizes, when one
    /// component is the checker's
    pub dt_run: Option<Box<DtRun>>,
    pub roles: Vec<RoleName>,
    /// the id of each name in [`Interner::roles`], by [`role_hash`], so that an object
    /// property is looked up without a copy of its IRI
    role_ids: hashbrown::HashTable<Role>,
    /// owned heap IRI bytes across `RoleName::Object`, `RoleName::Data`, and `RoleName::Inverse`
    role_name_bytes: usize,
    fresh: u32,
    /// the fresh classes of the Horn normalization
    fresh_classes: u32,
    /// the fresh classes each scope made, in the order it asked for them, with the run
    /// that last opened the scope, so that a later run over the same interner gives the
    /// scope the same classes (roadmap item 753)
    scopes: FxHashMap<Scope, (Vec<u32>, u32)>,
    /// the scope open now, with the number of fresh classes it has asked for
    scope: Option<(Scope, usize)>,
    /// the number of the run, which [`Interner::begin_run`] advances
    run: u32,
    /// the number of each axiom outside OWL 2 EL that a normalization opened a scope
    /// for, with the run that last did, an owned copy of those axioms
    scope_axioms: FxHashMap<AxiomKind, (u32, u32)>,
    next_scope_axiom: u32,
    /// the estimated bytes of [`Interner::scope_axioms`] and [`Interner::scopes`]
    scope_bytes: usize,
    /// the fresh role of each chain prefix, so that a later run over the same interner
    /// gives the prefix the same role
    prefix_roles: FxHashMap<(Role, Role), Role>,
    /// the universals that the unfolding of a universal by an automaton made
    /// ([`Interner::made_universal`]) and no normalization has named since, which a
    /// later run over the same interner does not unfold again
    made_universals: FxHashSet<Ex>,
    /// whether this is the interner of a chunk of a parallel normalization, which keeps
    /// intersections whole as [`Node::And`]
    chunk: bool,
    /// a chunk's intersections: the operands, and whether it is the binary intersection
    /// of [`Interner::conj`] rather than that of a class expression
    pub ands: Vec<(Vec<Ex>, bool)>,
    and_ids: FxHashMap<(Vec<Ex>, bool), u32>,
    /// the operands of [`Interner::ands`], counted twice, once for the table's key
    and_ops: usize,
    /// Optional canonical individual map shared across the main interner and parallel
    /// chunk interners via `Arc` (item 1061 phase 2 constant merging).
    pub canon: Option<std::sync::Arc<CanonMap>>,
}

/// Exact heap allocation size of an `Arc<T>`'s `ArcInner<T>` control block and payload,
/// accounting for both atomic reference counts and target alignment.
pub fn arc_payload_bytes<T>() -> Option<usize> {
    let header = std::alloc::Layout::array::<std::sync::atomic::AtomicUsize>(2).ok()?;
    let (layout, _) = header.extend(std::alloc::Layout::new::<T>()).ok()?;
    Some(layout.pad_to_align().size())
}

/// Clones `n` into a fresh `NamedIndividual` whose backing `String` has exact capacity
/// equal to its byte length.
pub fn exact_individual(n: &NamedIndividual) -> NamedIndividual {
    NamedIndividual::new(n.as_str().to_owned())
}

/// Immutable canonical individual map shared across the main `Interner` and parallel
/// chunk interners via `Arc`, carrying exact retained and logical byte sizes for
/// Item 1291 memory accounting. Each representative string is stored once in `reps`.
pub struct CanonMap {
    reps: Vec<NamedIndividual>,
    map: FxHashMap<NamedIndividual, u32>,
    generated_hub: Option<NamedIndividual>,
    owned_bytes: usize,
    logical_bytes: usize,
}

impl CanonMap {
    pub fn new_from_parts(
        reps: Vec<NamedIndividual>,
        map: FxHashMap<NamedIndividual, u32>,
        generated_hub: Option<NamedIndividual>,
        owned_bytes: usize,
        logical_bytes: usize,
    ) -> Self {
        Self {
            reps,
            map,
            generated_hub,
            owned_bytes,
            logical_bytes,
        }
    }

    #[inline]
    pub fn generated_hub(&self) -> Option<&NamedIndividual> {
        self.generated_hub.as_ref()
    }

    #[inline]
    pub fn get(&self, i: &NamedIndividual) -> Option<&NamedIndividual> {
        self.map.get(i).map(|&r| &self.reps[r as usize])
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&NamedIndividual, &NamedIndividual)> + '_ {
        self.map.iter().map(|(k, &r)| (k, &self.reps[r as usize]))
    }

    #[inline]
    pub fn owned_bytes(&self) -> usize {
        self.owned_bytes
    }

    #[inline]
    pub fn logical_bytes(&self) -> usize {
        self.logical_bytes
    }
}

/// What a run of fresh classes is made for, which names them across the runs over one
/// interner (roadmap item 753).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// the Horn normalization of an axiom outside OWL 2 EL, by [`Interner::scope_axiom`]
    Horn(u32),
    /// the automaton states that the Horn rules unfold a universal or a negative
    /// existential restriction into, by that expression
    Unfold(Ex),
    /// Tier D's normalization of an axiom outside OWL 2 EL, by
    /// [`Interner::scope_axiom`]
    Dl(u32),
    /// the automaton states that Tier D's normalization unfolds a universal or a
    /// negative existential restriction into, by that expression
    DlUnfold(Ex),
    /// the hypothetical successor of an existential restriction, by its expression
    Hyp(Ex),
}

/// The estimated bytes of a scope's entry, its classes aside.
const SCOPE_BYTES: usize = 48;

/// The expressions that [`Index::changed`] compares between two polls of its stop.
pub(super) const POLL_CHANGED: usize = 4096;

#[inline]
fn tick_changed(ticks: &mut usize, stop: &dyn Fn() -> bool) -> bool {
    *ticks += 1;
    if *ticks >= POLL_CHANGED {
        *ticks = 0;
        stop()
    } else {
        false
    }
}

fn slice_eq_poll<T: PartialEq>(
    a: &[T],
    b: &[T],
    ticks: &mut usize,
    stop: &dyn Fn() -> bool,
) -> Option<bool> {
    if a.len() != b.len() {
        return Some(false);
    }
    let mut i = 0;
    while i < a.len() {
        let step = POLL_CHANGED.saturating_sub(*ticks).max(1);
        let end = (i + step).min(a.len());
        if a[i..end] != b[i..end] {
            return Some(false);
        }
        *ticks += end - i;
        i = end;
        if *ticks >= POLL_CHANGED {
            *ticks = 0;
            if stop() {
                return None;
            }
        }
    }
    Some(true)
}

struct ValueInsertionPlan {
    vector_capacity: usize,
    bytes: usize,
}

fn next_value_capacity(len: usize, capacity: usize) -> Option<usize> {
    if len < capacity {
        Some(capacity)
    } else {
        let needed = len.checked_add(1)?;
        Some(capacity.checked_mul(2)?.max(4).max(needed))
    }
}

impl Interner {
    /// Bytes allocated by [`Interner::ensure_top_bottom`] (`nodes` initial capacity 4 × 40).
    pub const EMPTY_INIT_BYTES: usize = 4 * 40;

    pub fn new() -> Interner {
        let mut i = Interner::default();
        i.ensure_top_bottom();
        i
    }

    /// Initialises `Node::Top` and `Node::Bottom` on a zero-allocation default interner.
    pub fn ensure_top_bottom(&mut self) {
        if self.nodes.is_empty() {
            self.node(Node::Top);
            self.node(Node::Bottom);
        }
    }

    /// The interner of a chunk of a parallel normalization, with the datatype
    /// definitions of the run.
    pub fn chunk(definitions: &FxHashMap<Datatype, DataRange>) -> Interner {
        let mut i = Interner::new();
        i.chunk = true;
        i.ranges.definitions = definitions.clone();
        i
    }

    /// Retained inline data containers and their independently owned opaque arrays.
    pub fn data_bytes(&self) -> usize {
        self.checked_data_bytes().unwrap_or(usize::MAX)
    }

    /// Logical data-value storage estimate, independent of allocator capacities.
    pub fn logical_data_bytes(&self) -> usize {
        let vector = self
            .vals
            .len()
            .saturating_mul(std::mem::size_of::<DataVal>());
        let table =
            super::saturate::table_bytes(self.val_ids.len(), std::mem::size_of::<(DataVal, u32)>());
        let opaque: usize = self
            .vals
            .iter()
            .map(DataVal::logical_owned_bytes)
            .sum::<usize>()
            .saturating_mul(2);
        vector.saturating_add(table).saturating_add(opaque)
    }

    fn checked_data_bytes(&self) -> Option<usize> {
        let vector = self
            .vals
            .capacity()
            .checked_mul(std::mem::size_of::<DataVal>())?;
        let table = self
            .val_ids
            .capacity()
            .checked_mul(std::mem::size_of::<(DataVal, u32)>().checked_add(1)?)?
            .checked_mul(8)?
            / 7;
        vector
            .checked_add(table)?
            .checked_add(self.val_opaque_bytes)?
            .checked_add(self.dt.bytes())
    }

    /// Exact retained bytes of the shared canonical individual map on the main interner
    /// (`0` on parallel chunk interners that share the same `Arc`).
    pub fn canon_bytes(&self) -> usize {
        if self.chunk {
            0
        } else {
            self.canon.as_ref().map_or(0, |c| c.owned_bytes())
        }
    }

    /// O(1) bytes of the interner excluding [`Interner::dt_run`].
    pub fn core_bytes(&self) -> usize {
        self.data_bytes()
            + self.nodes.capacity() * 40
            + self.classes.bytes()
            + self.classes.name_bytes()
            + self.met.capacity()
            + self.class_nodes.capacity() * 4
            + self.individuals.bytes()
            + self.individuals.name_bytes()
            + self.individual_nodes.capacity() * 4
            + self.roles.capacity() * 48
            + self.role_name_bytes
            + self.ands.len() * 80
            + self.and_ops * 4
            + self.scope_bytes()
            + self.prefix_roles.capacity() * 16
    }

    /// Bytes of the immutable `dt_run` tables (`comps` and `defs.map`).
    pub fn dt_run_static_bytes(&self) -> usize {
        self.dt_run.as_ref().map_or(0, |r| r.static_bytes())
    }

    /// O(1) bytes of the mutable `dt_run` conversion memo table.
    pub fn dt_run_memo_bytes(&self) -> usize {
        self.dt_run.as_ref().map_or(0, |r| r.memo_bytes())
    }

    /// The bytes the interner takes, roughly: each expression with its entry in the
    /// table, the names with their IRIs, and a chunk's intersections.
    pub fn bytes(&self) -> usize {
        self.core_bytes() + self.dt_run.as_ref().map_or(0, |r| r.bytes()) + self.canon_bytes()
    }

    /// Whether a chunk met data ranges or literals, whose ids its merge does not map, so
    /// that the run normalizes on one thread instead.
    pub fn chunk_has_data(&self) -> bool {
        !self.vals.is_empty() || !self.ranges.opaque.is_empty() || !self.dt.is_empty()
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
                    Node::DataSome(..) | Node::DtAtom(_) | Node::DtFire(_) => {
                        unreachable!("a chunk with data merges on one thread")
                    }
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
        let i = self.canon.as_ref().and_then(|m| m.get(i)).unwrap_or(i);
        self.individuals.id(i)
    }

    /// The role of a name, if the interner has it.
    pub fn role_id(&self, name: &RoleName) -> Option<Role> {
        self.role_ids
            .find(role_hash(name), |&id| self.roles[id as usize] == *name)
            .copied()
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
            // a normalization names a universal that an automaton made, so it is
            // unfolded from here on, as in a run that starts again
            if !self.made_universals.is_empty() && matches!(n, Node::Forall(..)) {
                self.made_universals.remove(&e);
            }
            return e;
        }
        let e = self.nodes.len() as Ex;
        self.nodes.push(n);
        self.map.insert(n, e);
        e
    }

    /// The universal `∀r.x` of a transition of an automaton that unfolds a universal.
    /// One that the interner did not hold is marked as made by an automaton, and a
    /// run does not unfold a marked universal ([`Interner::made_by_automaton`]). A
    /// run that starts again holds none of them when it chooses the universals to
    /// unfold, and a kept interner holds those of the runs before.
    pub fn made_universal(&mut self, r: Role, x: Ex) -> Ex {
        let n = Node::Forall(r, x);
        if let Some(&e) = self.map.get(&n) {
            return e;
        }
        let e = self.nodes.len() as Ex;
        self.nodes.push(n);
        self.map.insert(n, e);
        self.made_universals.insert(e);
        e
    }

    /// Whether an automaton made the universal and no normalization has named it.
    pub fn made_by_automaton(&self, e: Ex) -> bool {
        self.made_universals.contains(&e)
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

    /// Takes the classes whose IRIs start with `prefix` out of the signature, which the
    /// run's reductions made up (item 805).
    pub fn hide_prefix(&mut self, prefix: &str) {
        for id in 0..self.met.len() {
            if self.met[id] && self.classes[id].as_str().starts_with(prefix) {
                self.met[id] = false;
            }
        }
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

    /// The nominal of a named or an anonymous individual. An anonymous individual is a
    /// fresh individual of the run (roadmap item 1281): in OWL 2 an ontology holds when
    /// some element stands for each of its anonymous individuals, so the classifier names
    /// each one, and the entailments that do not name it are those of the ontology. Its
    /// label is the one of the read, which names one individual wherever it occurs among
    /// the run's axioms, as it makes equal axioms one. Its name in the interner is
    /// `_:label`, which no IRI starts with, since the scheme of an IRI starts with a
    /// letter ([`is_anonymous`]).
    pub fn individual(&mut self, i: &Individual) -> Ex {
        match i {
            Individual::Named(n) => self.nominal(n),
            Individual::Anonymous(a) => self.nominal(&anon_individual(a)),
        }
    }

    /// The id of a data range.
    pub fn val(&mut self, v: DataVal) -> u32 {
        match self.val_ids.get(&v) {
            Some(&id) => id,
            None => {
                let before = self.checked_data_bytes();
                let plan = self.value_insertion_plan(&v);
                let Ok(reservation) = self
                    .ranges
                    .reserve_storage(before.and_then(|_| plan.as_ref().map(|p| p.bytes)))
                else {
                    return 0;
                };
                // A successful reservation proves both checked computations succeeded.
                let (before, plan) = (before.unwrap(), plan.unwrap());
                if plan.vector_capacity > self.vals.capacity() {
                    self.vals
                        .reserve_exact(plan.vector_capacity - self.vals.len());
                }
                self.val_ids.reserve(1);
                let id = self.vals.len() as u32;
                let copy = v.clone();
                #[cfg(test)]
                a1_record_accounting_visit();
                let copied = copy.owned_bytes();
                #[cfg(test)]
                a1_record_accounting_visit();
                let moved = v.owned_bytes();
                // Both capacities were bounded in the plan before either allocation.
                // Retain a terminal overflow sentinel if an allocator breaks that bound.
                self.val_opaque_bytes = self
                    .val_opaque_bytes
                    .checked_add(copied)
                    .and_then(|n| n.checked_add(moved))
                    .unwrap_or(usize::MAX);
                self.vals.push(copy);
                self.val_ids.insert(v, id);
                let retained = self
                    .checked_data_bytes()
                    .and_then(|n| n.checked_sub(before));
                if !self
                    .ranges
                    .retain_storage(reservation, retained, plan.bytes)
                {
                    return 0;
                }
                id
            }
        }
    }

    fn value_insertion_plan(&self, v: &DataVal) -> Option<ValueInsertionPlan> {
        let vector_capacity = next_value_capacity(self.vals.len(), self.vals.capacity())?;
        // The entire replacement buffer overlaps the old buffer, already in base.
        let vector = if vector_capacity > self.vals.capacity() {
            std::alloc::Layout::array::<DataVal>(vector_capacity)
                .ok()?
                .size()
        } else {
            0
        };
        // Keep the conservative allowance for a geometric map rehash, including
        // the compiled tuple and control-byte sizes. No rehash needs new credit
        // after allocation; its actual retained capacity is transferred below.
        let map = if self.val_ids.len() == self.val_ids.capacity() {
            self.val_ids
                .capacity()
                .checked_add(4)?
                .checked_mul(4)?
                .checked_mul(std::mem::size_of::<(DataVal, u32)>().checked_add(1)?)?
        } else {
            0
        };
        let opaque = v
            .opaque
            .capacity()
            .checked_add(v.opaque.len())?
            .checked_mul(std::mem::size_of::<u32>())?;
        self.val_opaque_bytes.checked_add(opaque)?;
        Some(ValueInsertionPlan {
            vector_capacity,
            bytes: vector.checked_add(map)?.checked_add(opaque)?,
        })
    }

    fn copy_values(&mut self) -> Result<Vec<DataVal>, Halt> {
        self.ranges.check_halt()?;
        #[cfg(test)]
        if let Some(b) = &self.ranges.budget {
            b.at(super::numeric::Point::IndexCopy)
                .map_err(|e| match e {
                    super::Error::Memory { used, .. } => Halt::Memory(used),
                    _ => Halt::Stopped,
                })?;
        }
        if !self.ranges.admit_copies(&self.vals) {
            self.ranges.check_halt()?;
        }
        Ok(self.vals.clone())
    }

    /// The expressions of the individuals.
    pub fn nominals(&self) -> impl Iterator<Item = Ex> + '_ {
        (0..self.individuals.len() as u32).filter_map(|i| self.get(&Node::Nominal(i)))
    }

    /// The expressions of the anonymous individuals ([`Interner::individual`]), sorted.
    pub fn anonymous_nominals(&self) -> Vec<Ex> {
        let mut v: Vec<Ex> = self
            .individuals
            .iter()
            .enumerate()
            .filter(|(_, n)| is_anonymous(n))
            .filter_map(|(i, _)| self.get(&Node::Nominal(i as u32)))
            .collect();
        v.sort_unstable();
        v
    }

    pub fn role(&mut self, r: RoleName) -> Role {
        if let Some(id) = self.role_id(&r) {
            return id;
        }
        let id = self.roles.len() as Role;
        let h = role_hash(&r);
        let name_len = match &r {
            RoleName::Object(p) | RoleName::Inverse(p) => p.as_str().len(),
            RoleName::Data(p) => p.as_str().len(),
            RoleName::Fresh(_) => 0,
        };
        self.role_name_bytes = self.role_name_bytes.saturating_add(name_len);
        self.roles.push(r);
        let roles = &self.roles;
        self.role_ids
            .insert_unique(h, id, |&i| role_hash(&roles[i as usize]));
        id
    }

    pub(super) fn object_role_id(&self, p: &ObjectProperty) -> Option<Role> {
        self.role_ids
            .find(
                object_hash(p.as_str()),
                |&id| matches!(&self.roles[id as usize], RoleName::Object(q) if q == p),
            )
            .copied()
    }

    /// The role of an object property, which copies its IRI only when it is new.
    pub fn object_role(&mut self, p: &ObjectProperty) -> Role {
        self.object_role_id(p)
            .unwrap_or_else(|| self.role(RoleName::Object(p.clone())))
    }

    pub fn fresh_role(&mut self) -> Role {
        self.fresh += 1;
        self.role(RoleName::Fresh(self.fresh))
    }

    /// The fresh role of the chain prefix `left ∘ r`, the same in every run over this
    /// interner.
    pub fn prefix_role(&mut self, left: Role, r: Role) -> Role {
        if let Some(&u) = self.prefix_roles.get(&(left, r)) {
            return u;
        }
        let u = self.fresh_role();
        self.prefix_roles.insert((left, r), u);
        u
    }

    /// A class that no axiom names, for the Horn normalization. Inside a scope
    /// ([`Interner::begin_scope`]) the k-th call gives the class that the k-th call of
    /// the same scope gave in an earlier run over this interner, so that the contexts
    /// kept from that run name the same classes. Outside a scope, and in a scope's
    /// first run, each call makes a new class.
    pub fn fresh_class(&mut self) -> Ex {
        if let Some((scope, at)) = self.scope.as_mut() {
            let k = *at;
            *at += 1;
            let scope = *scope;
            if let Some(&n) = self.scopes.get(&scope).and_then(|(made, _)| made.get(k)) {
                return self.node(Node::Fresh(n));
            }
            self.fresh_classes += 1;
            let n = self.fresh_classes;
            let (made, _) = self.scopes.entry(scope).or_default();
            debug_assert_eq!(made.len(), k);
            made.push(n);
            self.scope_bytes += 4;
            return self.node(Node::Fresh(n));
        }
        self.fresh_classes += 1;
        self.node(Node::Fresh(self.fresh_classes))
    }

    /// The estimated bytes of the scopes of fresh classes and their axioms, and of the
    /// marks of the universals that automata made.
    pub fn scope_bytes(&self) -> usize {
        self.scope_bytes + self.made_universals.capacity() * 5
    }

    pub fn logical_scope_bytes(&self) -> usize {
        self.scope_bytes + self.made_universals.len() * 5
    }

    /// Starts a run's normalization: the scopes it opens are those the run keeps.
    pub fn begin_run(&mut self) {
        self.run += 1;
        self.scope = None;
    }

    /// Opens a scope of fresh classes. A run opens each scope once at most.
    pub fn begin_scope(&mut self, scope: Scope) {
        debug_assert!(self.scope.is_none(), "a scope inside a scope");
        let run = self.run;
        let new = !self.scopes.contains_key(&scope);
        let e = self.scopes.entry(scope).or_default();
        e.1 = run;
        if new {
            self.scope_bytes += SCOPE_BYTES;
        }
        self.scope = Some((scope, 0));
    }

    pub fn end_scope(&mut self) {
        self.scope = None;
    }

    /// The number of an axiom outside OWL 2 EL in the scopes of its fresh classes, the
    /// same in every run over this interner that normalizes the axiom.
    pub fn scope_axiom(&mut self, k: &AxiomKind) -> u32 {
        let run = self.run;
        if let Some((n, seen)) = self.scope_axioms.get_mut(k) {
            *seen = run;
            return *n;
        }
        let n = self.next_scope_axiom;
        self.next_scope_axiom += 1;
        self.scope_bytes += super::held_bytes_of(k) + 16;
        self.scope_axioms.insert(k.clone(), (n, run));
        n
    }

    /// Drops the scopes and the axioms that the run did not open, at the end of its
    /// normalization.
    pub fn end_run(&mut self) {
        let run = self.run;
        self.scope = None;
        let mut freed = 0usize;
        self.scope_axioms.retain(|k, (_, seen)| {
            let keep = *seen == run;
            if !keep {
                freed += super::held_bytes_of(k) + 16;
            }
            keep
        });
        self.scopes.retain(|_, (made, seen)| {
            let keep = *seen == run;
            if !keep {
                freed += SCOPE_BYTES + made.len() * 4;
            }
            keep
        });
        self.scope_bytes = self.scope_bytes.saturating_sub(freed);
    }

    /// Whether the run's checker takes the data property's component.
    pub fn dt_checker(&self, d: &DataProperty) -> bool {
        self.dt_run.as_ref().is_some_and(|r| r.comps.checker(d))
    }

    /// The checker's atom `≥ n d.R` or `≤ n d.R` over a property of a checker component,
    /// with the forms that always or never hold folded to `owl:Thing` and `owl:Nothing`.
    /// Only the interned empty range folds, since another empty form can have another id.
    pub fn dt_node(&mut self, d: &DataProperty, at_least: bool, n: u64, range: Rid) -> Ex {
        match (at_least, n, range) {
            (true, 0, _) => return TOP,
            (true, _, DtRanges::EMPTY) => return BOTTOM,
            (false, _, DtRanges::EMPTY) | (false, u64::MAX, _) => return TOP,
            _ => {}
        }
        let role = self.role(RoleName::Data(d.clone()));
        let key = DtKey {
            role,
            at_least,
            n,
            range,
        };
        let id = if let Some(run) = self.dt_run.as_deref_mut() {
            match run.atom_id(&mut self.dt, key) {
                Some(id) => id,
                None => return TOP,
            }
        } else {
            self.dt.id(key)
        };
        self.node(Node::DtAtom(id))
    }

    /// The checker's range of a data range, with the run's definitions.
    pub fn dt_range(&mut self, r: &DataRange) -> Result<Rid, &'static str> {
        let run = self
            .dt_run
            .as_deref_mut()
            .expect("a checker range in a run with checker components");
        run.range(&mut self.dt, r)
    }

    /// The checker's point range of a literal.
    pub fn dt_literal(&mut self, l: &Literal) -> Result<Rid, &'static str> {
        let run = self
            .dt_run
            .as_deref_mut()
            .expect("a checker literal in a run with checker components");
        run.literal(&mut self.dt, l)
    }

    /// `∀d.R`, which is `≤ 0 d.¬R`.
    pub fn dt_universal(&mut self, d: &DataProperty, r: &DataRange) -> Result<Ex, &'static str> {
        let x = self.dt_range(r)?;
        let run = self
            .dt_run
            .as_deref_mut()
            .expect("a checker universal in a run with checker components");
        let not = run.not(&mut self.dt, x)?;
        Ok(self.dt_node(d, false, 0, not))
    }

    /// Whether a checker range conversion has latched a stop or memory halt.
    pub fn dt_stopped(&self) -> bool {
        self.dt_run.as_ref().is_some_and(|r| r.stopped.is_some())
    }

    /// Returns the latched checker stop or memory halt, if any.
    pub fn check_dt_stop(&self, limits: &super::Limits) -> Result<(), super::Error> {
        match self.dt_run.as_ref().and_then(|r| r.stopped) {
            Some(super::dtcheck::atoms::Halted::Timeout) => Err(super::Error::Timeout),
            Some(super::dtcheck::atoms::Halted::Cancelled) => Err(super::Error::Cancelled),
            Some(super::dtcheck::atoms::Halted::Memory(used)) => Err(super::Error::Memory {
                limit: limits.max_memory.unwrap_or(usize::MAX),
                used,
            }),
            None => Ok(()),
        }
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
#[derive(Default, PartialEq)]
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
    /// whether any data role has a functional superrole
    pub any_functional: bool,
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
    /// for each role, whether its chains compose left-linearly
    /// ([`Roles::finish_chains`], roadmap item 956)
    pub linear: Vec<bool>,
    /// for each pair of [`Roles::comp`], what the source of the composed links takes:
    /// the roles of those it keeps as forward links, and [`NO_ROLE`] for each that is
    /// empty, in the order of the composition table
    pub comp_fwd: FxHashMap<(Role, Role), Vec<Role>>,
}

/// The variable that turns the left-linear composition of [`Roles::finish_chains`] off
/// when it is `0`, for measurements (roadmap item 956).
pub const LINEAR_CHAINS_ENV: &str = "SPARKLES_LINEAR_CHAINS";

/// No role, in [`Roles::inv`].
pub const NO_ROLE: Role = Role::MAX;

impl Roles {
    /// The bytes the tables take, roughly, polling `tick` across each per-role table walk.
    pub fn bytes_poll<E>(&self, tick: &mut impl FnMut(usize) -> Result<(), E>) -> Result<usize, E> {
        let mut total = self.right.len()
            + self.left.len()
            + self.reflexive.len() * 4
            + self.range.len() * 8
            + self.data_range.len() * 8
            + self.empty.len()
            + self.linear.len();
        for v in &self.sup {
            tick(1)?;
            total += 24 + v.capacity() * 4;
        }
        for v in self.comp.values() {
            tick(1)?;
            total += 40 + v.capacity() * 4;
        }
        for v in &self.functional {
            tick(1)?;
            total += 24 + v.capacity() * 4;
        }
        for v in self.comp_fwd.values() {
            tick(1)?;
            total += 40 + v.capacity() * 4;
        }
        Ok(total)
    }

    pub fn logical_bytes(&self) -> usize {
        self.sup.iter().map(|v| 24 + v.len() * 4).sum::<usize>()
            + self.comp.values().map(|v| 40 + v.len() * 4).sum::<usize>()
            + self.right.len()
            + self.left.len()
            + self.reflexive.len() * 4
            + self.range.len() * 8
            + self.data_range.len() * 8
            + self
                .functional
                .iter()
                .map(|v| 24 + v.len() * 4)
                .sum::<usize>()
            + self.empty.len()
            + self.linear.len()
            + self
                .comp_fwd
                .values()
                .map(|v| 40 + v.len() * 4)
                .sum::<usize>()
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

    /// What the source of the links that [`Roles::comp`] composes takes, one message
    /// each: a forward link by each role that keeps them, less the left-linear
    /// compositions of [`Roles::finish_chains`], and the bottom, as [`NO_ROLE`], for each
    /// empty role.
    #[inline]
    pub fn comp_fwd(&self, r1: Role, r2: Role) -> &[Role] {
        if !self.left.get(r1 as usize).copied().unwrap_or(false) {
            return &[];
        }
        self.comp_fwd.get(&(r1, r2)).map_or(&[], |v| v.as_slice())
    }

    /// Composes every chain as before roadmap item 956, with every composed link kept as
    /// a forward link where its role keeps them, for tests that compare the two.
    #[cfg(test)]
    pub fn without_linear(&mut self) {
        self.linear.iter_mut().for_each(|l| *l = false);
        let (right, empty) = (&self.right, &self.empty);
        self.comp_fwd = self
            .comp
            .iter()
            .map(|(&k, v)| {
                let kept = v
                    .iter()
                    .flat_map(|&t| {
                        let fwd = right.get(t as usize).copied().unwrap_or(false).then_some(t);
                        let bottom = empty
                            .get(t as usize)
                            .copied()
                            .unwrap_or(false)
                            .then_some(NO_ROLE);
                        fwd.into_iter().chain(bottom)
                    })
                    .collect();
                (k, kept)
            })
            .collect();
    }

    /// Whether `r` and `s` are equivalent roles.
    fn equivalent(&self, r: Role, s: Role) -> bool {
        self.sub(r, s) && self.sub(s, r)
    }

    /// Decides which roles compose left-linearly and fills [`Roles::comp_fwd`], once the
    /// composition table is final (roadmap item 956).
    ///
    /// A role `t` is linear when every pair of roles equivalent to `t` composes into the
    /// roles that `t` with itself does, and every role `s` that composes with a link by
    /// `t` on its right composes alike with the roles equivalent to `t`, into roles `V`
    /// each of which some role of `V` composed with `t` gives again. A link by `t` that
    /// a chain composes from two links by roles equivalent to `t` then needs no forward
    /// link at its source. A link `(w, s, x)` that would meet such a link `(x, t, z)`,
    /// composed from `(x, a, y)` and `(y, b, z)`, gives `(w, u, y)` for every `u` of `V`
    /// with `(x, a, y)`, directly or by induction on the composition when that link is one
    /// of them too, and these with `(y, b, z)` give every role of `V` again. So the
    /// saturation stores the same backward links, a middle context pairs its backward
    /// links only with the forward links that are not such compositions, and the
    /// transitive closure of a role is derived once for each link that ends in a link
    /// the axioms give, rather than once for each intermediate context.
    /// `grow` takes the bytes of the tables it adds before it adds them.
    pub fn finish_chains<E>(
        &mut self,
        mut grow: impl FnMut(usize) -> Result<(), E>,
    ) -> Result<(), E> {
        let n = self.sup.len();
        let mut linear = vec![false; n];
        // a switch for measurements, which composes every pair as before
        static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let off =
            *OFF.get_or_init(|| std::env::var_os(LINEAR_CHAINS_ENV).is_some_and(|v| v == "0"));
        // the roles that two of their own links compose into, and the pairs with a link
        // by each of them on the right, which one pass over the table finds
        let mut right_of: FxHashMap<Role, Vec<Role>> = FxHashMap::default();
        if !off {
            for t in 0..n as Role {
                if self.keeps_forward(t) && self.comp(t, t).contains(&t) {
                    right_of.insert(t, Vec::new());
                }
            }
        }
        if !right_of.is_empty() {
            for (&(s, b), v) in &self.comp {
                if let Some(l) = right_of.get_mut(&b)
                    && !v.is_empty()
                    && self.left[s as usize]
                {
                    l.push(s);
                }
            }
        }
        for (&t, lefts) in &right_of {
            let class: Vec<Role> = self.sup[t as usize]
                .iter()
                .copied()
                .filter(|&s| self.sub(s, t))
                .collect();
            let sorted = |v: &[Role]| {
                let mut v = v.to_vec();
                v.sort_unstable();
                v
            };
            let tt = sorted(self.comp(t, t));
            // every pair of roles equivalent to t composes as t with itself
            let uniform = class
                .iter()
                .all(|&a| class.iter().all(|&b| sorted(self.comp(a, b)) == tt));
            // a role that composes with a link by t on its right does so alike with the
            // roles equivalent to t, and each role it composes into comes again from one
            // of them composed with a further link by t
            let covered = lefts.iter().all(|&s| {
                let st = sorted(self.comp(s, t));
                class.iter().all(|&a| sorted(self.comp(s, a)) == st)
                    && st
                        .iter()
                        .all(|w| st.iter().any(|&u| self.comp(u, t).contains(w)))
            });
            linear[t as usize] = uniform && covered;
        }
        self.linear = linear;
        grow(n)?;
        // a pair whose source takes nothing has no entry, and each entry is charged
        // before it is made
        let mut comp_fwd: FxHashMap<(Role, Role), Vec<Role>> = FxHashMap::default();
        for (&(r1, r2), v) in &self.comp {
            let products = |t: Role| {
                let fwd = self.keeps_composed(r1, r2, t).then_some(t);
                let bottom = self.is_empty_role(t).then_some(NO_ROLE);
                fwd.into_iter().chain(bottom)
            };
            let len = v.iter().map(|&t| products(t).count()).sum::<usize>();
            if len == 0 {
                continue;
            }
            grow(40 + len * 4)?;
            comp_fwd.insert((r1, r2), v.iter().flat_map(|&t| products(t)).collect());
        }
        self.comp_fwd = comp_fwd;
        Ok(())
    }

    /// Whether the source of a link by `t` that a chain composes from links by `r1` and
    /// `r2` keeps it as a forward link ([`Roles::comp_fwd`]).
    #[inline]
    pub fn keeps_composed(&self, r1: Role, r2: Role, t: Role) -> bool {
        self.keeps_forward(t)
            && !(self.linear.get(t as usize).copied().unwrap_or(false)
                && self.equivalent(r1, t)
                && self.equivalent(r2, t))
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
    /// `range(d) = R`, for a data role of a checker component and the checker's range
    pub dt_ranges: Vec<(Role, Rid)>,
    /// the pairs of disjoint data roles of checker components
    pub dt_disjoint: Vec<(Role, Role)>,
    /// `HasKey(c (p…) (d…))`
    pub keys: Vec<Key>,
    /// the individuals of assertions, which classification checks for consistency
    pub individuals: Vec<Ex>,
    /// the classes of the signature
    pub classes: Vec<Ex>,
    /// what the Horn normalization adds, when the run takes axioms outside OWL 2 EL
    pub horn: Option<Box<super::horn::HornAx>>,
    /// the clauses of Tier D, when the run takes disjunctive axioms
    pub dl: Option<Box<super::dl_norm::DlAx>>,
}

impl Axioms {
    /// Appends a chunk's normalized axioms to these, with the chunk's ids of
    /// expressions and roles mapped to the merged interner's. The disjointness sets are
    /// sorted by id, as the normalization sorts them.
    pub fn append_mapped(&mut self, chunk: Axioms, nodes: &[Ex], roles: &[Role]) {
        let n = |e: Ex| nodes[e as usize];
        let r = |x: Role| roles[x as usize];
        debug_assert!(
            chunk.data_ranges.is_empty()
                && chunk.dt_ranges.is_empty()
                && chunk.dt_disjoint.is_empty()
        );
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
            + self.dt_ranges.capacity() * 8
            + self.dt_disjoint.capacity() * 8
            + self.functional.capacity() * 4
            + self.keys.capacity() * 72
            + self.individuals.capacity() * 4
            + self.classes.capacity() * 4
    }

    /// The bytes the normalized axioms take.
    pub fn bytes(&self) -> usize {
        self.bytes_poll(&mut |_| Ok::<(), std::convert::Infallible>(()))
            .unwrap_or_else(|e| match e {})
    }

    /// Like [`Axioms::bytes`], polling `tick` across variable-length tables.
    pub fn bytes_poll<E>(&self, tick: &mut impl FnMut(usize) -> Result<(), E>) -> Result<usize, E> {
        let mut total = self.subs.capacity() * 8
            + self.disjoint.capacity() * 24
            + self.role_subs.capacity() * 8
            + self.reflexive.capacity() * 4
            + self.ranges.capacity() * 8
            + self.data_ranges.capacity() * 8
            + self.dt_ranges.capacity() * 8
            + self.dt_disjoint.capacity() * 8
            + self.functional.capacity() * 4
            + self.individuals.capacity() * 4
            + self.classes.capacity() * 4;
        for d in &self.disjoint {
            tick(1)?;
            total += 24 + d.capacity() * 4;
        }
        for (c, _) in &self.chains {
            tick(1)?;
            total += 32 + c.capacity() * 4;
        }
        for k in &self.keys {
            tick(1)?;
            total += 56 + (k.objects.len() + k.data.len()) * 4;
        }
        if let Some(h) = &self.horn {
            total += h.bytes_poll(tick)?;
        }
        if let Some(d) = &self.dl {
            total += d.bytes_poll(tick)?;
        }
        Ok(total)
    }

    /// Logical input-size measure of the normalized axioms, independent of vector
    /// capacities.
    pub fn logical_bytes(&self) -> usize {
        self.subs.len() * 8
            + self
                .disjoint
                .iter()
                .map(|d| 24 + d.len() * 4)
                .sum::<usize>()
            + self.disjoint.len() * 24
            + self.role_subs.len() * 8
            + self
                .chains
                .iter()
                .map(|(c, _)| 32 + c.len() * 4)
                .sum::<usize>()
            + self.reflexive.len() * 4
            + self.ranges.len() * 8
            + self.data_ranges.len() * 8
            + self.dt_ranges.len() * 8
            + self.dt_disjoint.len() * 8
            + self.functional.len() * 4
            + self
                .keys
                .iter()
                .map(|k| 56 + (k.objects.len() + k.data.len()) * 4)
                .sum::<usize>()
            + self.individuals.len() * 4
            + self.classes.len() * 4
            + self.horn.as_ref().map_or(0, |h| h.logical_bytes())
            + self.dl.as_ref().map_or(0, |d| d.logical_bytes())
    }
}

/// Maps one axiom's logical content into the interner. `out` collects what it means,
/// and the answer says what of it the classifier may not fully take into account. An
/// axiom with an expression the classifier cannot represent adds nothing to `out`.
///
/// Every class of the axiom's signature is interned, and so met for the run: those the
/// normalization does not reach are interned from the signature.
pub fn normalize(ax: &AxiomKind, it: &mut Interner, out: &mut Axioms) -> Option<&'static str> {
    let normalized = normalize_axiom(ax, it, out);
    // A guarded resource stop is propagated by the caller, never interned/reported
    // as an unsupported literal or a successful stand-in.
    if it.ranges.check().is_err() {
        return None;
    }
    let (partial, whole) = match normalized {
        Ok((partial, whole)) => (partial, whole),
        Err(why) => (Some(why), false),
    };
    if !whole {
        intern_signature(ax, it);
    }
    partial
}

/// Whether the axiom is an inclusion in the top object property or the top data
/// property, `r ⊑ U`, whatever its subproperty expression: it holds of every ontology,
/// and the role table puts every role below a top property that another axiom names
/// (roadmap item 1282). The classifier takes it as an axiom of OWL 2 EL that adds
/// nothing, and does not intern the top property for it, so that it keeps no run from
/// the tiers that the top property closes.
pub fn top_inclusion(ax: &AxiomKind) -> bool {
    match ax {
        AxiomKind::SubObjectPropertyOf(_, sup) => {
            sup.named().as_str() == vocab::OWL_TOP_OBJECT_PROPERTY
        }
        AxiomKind::SubDataPropertyOf(_, sup) => sup.as_str() == vocab::OWL_TOP_DATA_PROPERTY,
        _ => false,
    }
}

/// The start of the interner's name for an anonymous individual ([`Interner::individual`]).
pub const ANONYMOUS: &str = "_:";

/// The interner's canonical `NamedIndividual` spelling for an anonymous individual,
/// allocated with exact string capacity.
pub fn anon_individual(a: &AnonymousIndividual) -> NamedIndividual {
    let mut s = String::with_capacity(ANONYMOUS.len() + a.label().len());
    s.push_str(ANONYMOUS);
    s.push_str(a.label());
    NamedIndividual::new(s)
}

/// Whether the interner's individual stands for an anonymous individual, which no answer
/// names.
pub fn is_anonymous(i: &NamedIndividual) -> bool {
    i.as_str().starts_with(ANONYMOUS)
}

/// Whether the axiom holds an individual outside its annotations, named or anonymous
/// (roadmap item 1281).
pub fn holds_individual(ax: &AxiomKind) -> bool {
    crate::model::Signature::names_individual(ax)
        || crate::profiles::individuals(ax)
            .iter()
            .any(|i| matches!(i, Individual::Anonymous(_)))
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
    if top_inclusion(ax) {
        return Ok((None, true));
    }
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
            for w in es.windows(2) {
                if w[0] == w[1] {
                    out.subs.push((w[0], BOTTOM));
                }
            }
            es.dedup();
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
        K::SubDataPropertyOf(a, b)
            if (b.as_str() == vocab::OWL_BOTTOM_DATA_PROPERTY && it.dt_checker(a))
                || (a.as_str() == vocab::OWL_TOP_DATA_PROPERTY && it.dt_checker(b)) =>
        {
            return Err(DT_TOP);
        }
        K::EquivalentDataProperties(ps)
            if it.dt_run.is_some()
                && ps.iter().any(|p| it.dt_checker(p))
                && ps.iter().any(|p| {
                    matches!(
                        p.as_str(),
                        vocab::OWL_TOP_DATA_PROPERTY | vocab::OWL_BOTTOM_DATA_PROPERTY
                    )
                }) =>
        {
            return Err(DT_TOP);
        }
        K::DataPropertyDomain(d, _)
        | K::DataPropertyRange(d, _)
        | K::FunctionalDataProperty(d)
        | K::DataPropertyAssertion(d, _, _)
        | K::NegativeDataPropertyAssertion(d, _, _)
            if it.dt_run.is_some() && d.as_str() == vocab::OWL_TOP_DATA_PROPERTY =>
        {
            return Err(DT_TOP);
        }
        K::DataPropertyDomain(d, c) if it.dt_checker(d) => {
            let c = ce(c, it, &mut partial)?;
            let e = it.dt_node(d, true, 1, DtRanges::FULL);
            out.subs.push((e, c));
        }
        K::DataPropertyRange(d, range) if it.dt_checker(d) => {
            let x = it.dt_range(range)?;
            let r = it.role(RoleName::Data(d.clone()));
            out.dt_ranges.push((r, x));
        }
        K::FunctionalDataProperty(d) if it.dt_checker(d) => {
            let r = it.role(RoleName::Data(d.clone()));
            out.functional.push(r);
        }
        K::DataPropertyAssertion(d, a, v) if it.dt_checker(d) => {
            let x = it.dt_literal(v)?;
            let e = it.dt_node(d, true, 1, x);
            let a = it.individual(a);
            out.individuals.push(a);
            out.subs.push((a, e));
        }
        // ¬d(a, v) is `{a} ⊑ ∀d.¬{v}`, which is `{a} ⊑ ≤ 0 d.{v}`
        K::NegativeDataPropertyAssertion(d, a, v) if it.dt_checker(d) => {
            let x = it.dt_literal(v)?;
            let e = it.dt_node(d, false, 0, x);
            let a = it.individual(a);
            out.individuals.push(a);
            out.subs.push((a, e));
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
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
            let e = it.node(Node::DataSome(r, top));
            out.subs.push((e, c));
        }
        K::DataPropertyRange(d, range) => {
            let r = it.role(RoleName::Data(d.clone()));
            let (v, opaque) = it.ranges.range(range);
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
            if opaque {
                partial = Some(OPAQUE);
            }
            let v = it.val(v);
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
            out.data_ranges.push((r, v));
        }
        K::FunctionalDataProperty(d) => {
            let r = it.role(RoleName::Data(d.clone()));
            out.functional.push(r);
        }
        // the definitions are read before the axioms, and the data ranges that name a
        // defined datatype stand for its definition
        K::DatatypeDefinition(d, range) => {
            if it
                .dt_run
                .as_ref()
                .is_some_and(|run| run.defs.map().get(d).map(std::sync::Arc::as_ref) != Some(range))
            {
                partial = Some("another definition of the same datatype supersedes this one");
            }
            if it.ranges.range(range).1 {
                partial = Some(OPAQUE);
            }
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
        }
        K::HasKey(c, ops, dps) => {
            let class = ce(c, it, &mut partial)?;
            // the keys read the lattice's values, not the checker's atoms
            if dps.iter().any(|d| it.dt_checker(d)) {
                partial = Some(DT_KEY);
            }
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
        K::ClassAssertion(c, i) => {
            let c = ce(c, it, &mut partial)?;
            let n = it.individual(i);
            out.individuals.push(n);
            out.subs.push((n, c));
        }
        K::ObjectPropertyAssertion(p, a, b) => {
            if let Some(r) = object_role(p, it) {
                let a = it.individual(a);
                let b = it.individual(b);
                let e = it.node(Node::Exist(r, b));
                out.individuals.push(a);
                out.individuals.push(b);
                out.subs.push((a, e));
            }
        }
        // ¬p(a, b) is {a} ⊓ ∃p.{b} ⊑ ⊥
        K::NegativeObjectPropertyAssertion(p, a, b) => {
            let Some(r) = object_role(p, it) else {
                return Err("inverse properties");
            };
            let a = it.individual(a);
            let b = it.individual(b);
            let e = it.node(Node::Exist(r, b));
            let both = it.conj(a, e);
            out.individuals.push(a);
            out.individuals.push(b);
            out.subs.push((both, BOTTOM));
        }
        K::DataPropertyAssertion(d, a, v) => {
            let r = it.role(RoleName::Data(d.clone()));
            let a = it.individual(a);
            let (v, opaque) = it.ranges.literal(v);
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
            if opaque {
                partial = Some(OPAQUE);
            }
            let v = it.val(v);
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
            let e = it.node(Node::DataSome(r, v));
            out.individuals.push(a);
            out.subs.push((a, e));
        }
        K::NegativeDataPropertyAssertion(d, a, v) => {
            let r = it.role(RoleName::Data(d.clone()));
            let a = it.individual(a);
            let (v, opaque) = it.ranges.literal(v);
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
            if opaque {
                partial = Some(OPAQUE);
            }
            let v = it.val(v);
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
            let e = it.node(Node::DataSome(r, v));
            let both = it.conj(a, e);
            out.individuals.push(a);
            out.subs.push((both, BOTTOM));
        }
        K::SameIndividual(is) => {
            let es: Vec<Ex> = is.iter().map(|i| it.individual(i)).collect();
            out.individuals.extend(es.iter().copied());
            for w in 0..es.len() {
                let next = es[(w + 1) % es.len()];
                if es[w] != next {
                    out.subs.push((es[w], next));
                }
            }
        }
        K::DifferentIndividuals(is) => {
            let mut es: Vec<Ex> = is.iter().map(|i| it.individual(i)).collect();
            es.sort_unstable();
            let mut prev_dup: Option<Ex> = None;
            for w in es.windows(2) {
                if w[0] == w[1] && prev_dup != Some(w[0]) {
                    out.subs.push((w[0], BOTTOM));
                    prev_dup = Some(w[0]);
                }
            }
            es.dedup();
            out.individuals.extend(es.iter().copied());
            if es.len() >= 2 {
                out.disjoint.push(es);
            }
        }
        // annotations carry no logical content
        K::AnnotationAssertion(..)
        | K::SubAnnotationPropertyOf(..)
        | K::AnnotationPropertyDomain(..)
        | K::AnnotationPropertyRange(..) => {}
        // the profile check leaves the remaining types out of OWL 2 EL
        _ => whole = false,
    }
    Ok((partial, whole))
}

/// Why an axiom with a chain through the top object property is used in part: the
/// classifier composes links, and the top property links every pair.
pub(super) const TOP_CHAIN: &str = "property chains through the top object property";
/// Why an axiom with a literal or a datatype that the classifier cannot compare is used
/// in part: it is an atom that only equals itself.
pub const OPAQUE: &str = "literals and datatypes without values the classifier compares";

fn object_role(p: &ObjectPropertyExpression, it: &mut Interner) -> Option<Role> {
    match p {
        ObjectPropertyExpression::Property(p) => Some(it.object_role(p)),
        ObjectPropertyExpression::InverseOf(_) => None,
    }
}

/// The deepest nesting of class expressions that normalization follows, as the reverse
/// mapping bounds it.
const MAX_DEPTH: usize = 256;
const NUMERIC_HALTED: &str = "numeric construction halted";
/// A restriction over the top data property, or an axiom that makes a checker property
/// universal or empty, in a run with checker components. The top data property
/// reaches each component, which the checker reads one at a time.
pub const DT_TOP: &str =
    "a data restriction or axiom over the top or bottom data property beside the datatype checker";
/// A key over a property of a checker component, whose values the worlds do not compare.
pub const DT_KEY: &str = "a key over a data property that the datatype checker takes";

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
                    let r = it.object_role(p);
                    if f == BOTTOM {
                        return Ok((BOTTOM, partial));
                    }
                    (it.node(Node::Exist(r, f)), partial)
                }
                ObjectPropertyExpression::InverseOf(_) => return Err("inverse properties"),
            }
        }
        C::ObjectHasValue(p, i) => {
            let f = it.individual(i);
            match p {
                ObjectPropertyExpression::Property(p) => {
                    let r = it.object_role(p);
                    (it.node(Node::Exist(r, f)), None)
                }
                ObjectPropertyExpression::InverseOf(_) => return Err("inverse properties"),
            }
        }
        C::ObjectOneOf(is) if is.len() == 1 => match is.iter().next() {
            Some(i) => (it.individual(i), None),
            None => return Err("an empty enumeration"),
        },
        C::ObjectHasSelf(ObjectPropertyExpression::Property(p)) => {
            let r = it.object_role(p);
            (it.node(Node::SelfLoop(r)), None)
        }
        C::DataSomeValuesFrom(ps, _) | C::DataAllValuesFrom(ps, _)
            if ps.len() == 1
                && it.dt_run.is_some()
                && ps[0].as_str() == vocab::OWL_TOP_DATA_PROPERTY =>
        {
            return Err(DT_TOP);
        }
        C::DataHasValue(d, _)
            if it.dt_run.is_some() && d.as_str() == vocab::OWL_TOP_DATA_PROPERTY =>
        {
            return Err(DT_TOP);
        }
        C::DataSomeValuesFrom(ps, r) if ps.len() == 1 && it.dt_checker(&ps[0]) => {
            let x = it.dt_range(r)?;
            (it.dt_node(&ps[0], true, 1, x), None)
        }
        C::DataHasValue(d, l) if it.dt_checker(d) => {
            let x = it.dt_literal(l)?;
            (it.dt_node(d, true, 1, x), None)
        }
        C::DataSomeValuesFrom(ps, r) if ps.len() == 1 => {
            let role = it.role(RoleName::Data(ps[0].clone()));
            let (v, opaque) = it.ranges.range(r);
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
            let v = it.val(v);
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
            (it.node(Node::DataSome(role, v)), opaque.then_some(OPAQUE))
        }
        C::DataHasValue(d, l) => {
            let role = it.role(RoleName::Data(d.clone()));
            let (v, opaque) = it.ranges.literal(l);
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
            let v = it.val(v);
            it.ranges.check().map_err(|_| NUMERIC_HALTED)?;
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
    pub(super) top_chain: Option<super::top_chain::Rules>,
    /// Keeps the numeric accounting root alive without billing each alias.
    pub(super) numeric_ledger: Option<std::sync::Arc<super::numeric::NumericLedger>>,
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
    /// the expressions of the anonymous individuals among them, sorted, which a key does
    /// not bind (roadmap item 1281)
    pub anonymous: Vec<Ex>,
    /// whether a link to an individual can close a loop that a negative `ObjectHasSelf`
    /// reads
    pub nominal_selfs: bool,
    /// the top object and data roles, when the axioms use them
    pub tops: [Option<Role>; 2],
    /// what the Horn rules read, when the run takes axioms outside OWL 2 EL
    pub horn: Option<Box<super::horn::HornIx>>,
    /// what the rules of Tier D read, when the run takes disjunctive axioms
    pub dl: Option<Box<super::dl_rules::DlIx>>,
    /// what the checker's rule reads, when the run has checker components, whose atoms
    /// no other run's contexts read
    pub dt: Option<Box<super::dt_sat::DtIx>>,
}

impl Index {
    /// Whether `r` is above a top role, so that it relates every pair of elements.
    pub fn is_universal(&self, r: Role) -> bool {
        self.tops.iter().flatten().any(|&t| self.roles.sub(t, r))
    }

    /// Whether `e` is the expression of a named individual, which a key binds: a nominal
    /// that is not an anonymous individual's.
    pub fn named_nominal(&self, e: Ex) -> bool {
        matches!(self.head(e).node, Node::Nominal(_)) && self.anonymous.binary_search(&e).is_err()
    }
}

#[cfg(test)]
impl Index {
    pub(super) fn head_count(&self) -> usize {
        self.heads.len()
    }

    /// The fixture may replace only key records after a production index build.
    pub(super) fn assert_key_fixture_equivalent(&self, expected: &Index) {
        let Index {
            numeric_ledger: a_ledger,
            heads: a_heads,
            told: a_told,
            conj: a_conj,
            exist: a_exist,
            disjoint: a_disjoint,
            selfs: a_selfs,
            roles: a_roles,
            vals: a_vals,
            neg_data: a_neg_data,
            universal: a_universal,
            keys: a_keys,
            nominals: a_nominals,
            anonymous: a_anonymous,
            nominal_selfs: a_nominal_selfs,
            tops: a_tops,
            horn: a_horn,
            dl: a_dl,
            dt: a_dt,
            top_chain: a_top_chain,
        } = self;
        let Index {
            numeric_ledger: b_ledger,
            heads: b_heads,
            told: b_told,
            conj: b_conj,
            exist: b_exist,
            disjoint: b_disjoint,
            selfs: b_selfs,
            roles: b_roles,
            vals: b_vals,
            neg_data: b_neg_data,
            universal: b_universal,
            keys: b_keys,
            nominals: b_nominals,
            anonymous: b_anonymous,
            nominal_selfs: b_nominal_selfs,
            tops: b_tops,
            horn: b_horn,
            dl: b_dl,
            dt: b_dt,
            top_chain: b_top_chain,
        } = expected;
        assert!(a_top_chain.is_none() && b_top_chain.is_none());
        assert_eq!(a_dt.is_some(), b_dt.is_some());
        match (a_ledger, b_ledger) {
            (None, None) => {}
            (Some(a), Some(b)) => {
                assert!(
                    std::sync::Arc::ptr_eq(a, b),
                    "fixture Index ledger roots differ"
                );
                assert!(
                    std::sync::Arc::downgrade(a).upgrade().is_some(),
                    "fixture Index ledger is not live"
                );
            }
            _ => panic!("fixture Index ledger presence differs from production reference"),
        }
        assert_eq!(a_heads.len(), b_heads.len(), "fixture heads");
        let span = |s: &Span| {
            let Span { start, len } = s;
            (*start, *len)
        };
        for (a, b) in a_heads.iter().zip(b_heads) {
            let Head {
                node: an,
                succ: asucc,
                told: at,
                conj: ac,
                exist: ae,
                disj: ad,
            } = a;
            let Head {
                node: bn,
                succ: bsucc,
                told: bt,
                conj: bc,
                exist: be,
                disj: bd,
            } = b;
            assert_eq!(
                (an, asucc, span(at), span(ac), span(ae), span(ad)),
                (bn, bsucc, span(bt), span(bc), span(be), span(bd)),
                "fixture head content"
            );
        }
        assert_eq!(a_told, b_told, "fixture told");
        assert_eq!(a_conj, b_conj, "fixture conjunctions");
        assert_eq!(a_exist, b_exist, "fixture existentials");
        assert_eq!(a_disjoint, b_disjoint, "fixture disjoint sets");
        assert_eq!(a_selfs, b_selfs, "fixture selfs");
        let Roles {
            sup: a_sup,
            comp: a_comp,
            right: a_right,
            left: a_left,
            reflexive: a_reflexive,
            range: a_range,
            data_range: a_data_range,
            functional: a_functional,
            any_functional: a_any_functional,
            empty: a_empty,
            any_empty: a_any_empty,
            inv: a_inv,
            clash: a_clash,
            linear: a_linear,
            comp_fwd: a_comp_fwd,
        } = a_roles;
        let Roles {
            sup: b_sup,
            comp: b_comp,
            right: b_right,
            left: b_left,
            reflexive: b_reflexive,
            range: b_range,
            data_range: b_data_range,
            functional: b_functional,
            any_functional: b_any_functional,
            empty: b_empty,
            any_empty: b_any_empty,
            inv: b_inv,
            clash: b_clash,
            linear: b_linear,
            comp_fwd: b_comp_fwd,
        } = b_roles;
        assert_eq!(a_any_functional, b_any_functional, "fixture any_functional");
        assert_eq!(
            (
                a_sup,
                a_comp,
                a_right,
                a_left,
                a_reflexive,
                a_range,
                a_data_range,
                a_functional,
                a_empty,
                a_any_empty,
                a_inv,
                a_clash
            ),
            (
                b_sup,
                b_comp,
                b_right,
                b_left,
                b_reflexive,
                b_range,
                b_data_range,
                b_functional,
                b_empty,
                b_any_empty,
                b_inv,
                b_clash
            ),
            "fixture role content"
        );
        assert_eq!(
            (a_linear, a_comp_fwd),
            (b_linear, b_comp_fwd),
            "fixture linear chains"
        );
        assert_eq!(a_vals, b_vals, "fixture values");
        assert_eq!(a_neg_data, b_neg_data, "fixture negative data");
        assert_eq!(a_universal, b_universal, "fixture universal facts");
        assert_eq!(a_keys, b_keys, "fixture original keys including duplicates");
        assert_eq!(a_nominals, b_nominals, "fixture nominals");
        assert_eq!(a_anonymous, b_anonymous, "fixture anonymous individuals");
        assert_eq!(a_nominal_selfs, b_nominal_selfs, "fixture nominal selfs");
        assert_eq!(a_tops, b_tops, "fixture top roles");
        assert!(
            a_horn.is_none() && b_horn.is_none(),
            "fixture requires no Horn index"
        );
        assert!(
            a_dl.is_none() && b_dl.is_none(),
            "fixture requires no DL index"
        );
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

/// Like [`group`], sorting each key's slice in place so [`Index::changed`] can compare
/// per-head rule lists without allocating temporary copies.
fn group_sorted<T: Copy + Ord>(
    pairs: &[(Ex, T)],
    span: impl Fn(&mut Head) -> &mut Span,
    heads: &mut [Head],
) -> Vec<T> {
    let mut out = group(pairs, &span, heads);
    for h in heads.iter_mut() {
        let s = *span(h);
        if s.len > 1 {
            let start = s.start as usize;
            out[start..start + s.len as usize].sort_unstable();
        }
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

    /// Whether a later run may compare this index with its own
    /// ([`Index::same_tables`]): it has no individuals, universal roles, keys, or top
    /// data property, with which that comparison fails. Data restrictions (both the EL
    /// lattice, including functional data properties, and Item 972's datatype checker)
    /// are comparable across runs (roadmap item 753).
    pub fn comparable(&self) -> bool {
        self.universal.is_empty()
            && self.keys.is_empty()
            && self.nominals.is_empty()
            && !self.nominal_selfs
            && self.tops[1].is_none()
    }

    /// Whether a context that a run over `old` saturated reads the same tables here
    /// when it holds none of the expressions that [`Index::changed`] lists (roadmap
    /// item 753): the tables that no expression keys are equal on the roles of `old`,
    /// neither index has individuals, universal roles or keys, both agree on `plain` /
    /// `dominate` parity and datatype-checker mode assignment for every old data
    /// property, and both have the Horn rules or neither, and Tier D or neither, where
    /// Tier D's tables are those its expressions key
    /// ([`DlIx::keyed_by_expressions`](super::dl_rules::DlIx)).
    /// Roles that `old` lacks may be new here, since a kept context holds no link by
    /// them.
    #[allow(dead_code)]
    pub fn same_tables(&self, old: &Index) -> bool {
        self.same_tables_with_stop(old, &|| false).unwrap_or(false)
    }

    /// Cooperative polling variant of [`Index::same_tables`]; returns `None` when `stop`
    /// signals cancellation or deadline expiry.
    pub fn same_tables_with_stop(&self, old: &Index, stop: &dyn Fn() -> bool) -> Option<bool> {
        let (a, b) = (&old.roles, &self.roles);
        let n = a.sup.len();
        if b.sup.len() < n
            || b.right.len() < a.right.len()
            || b.left.len() < a.left.len()
            || a.reflexive.len() != b.reflexive.len()
            || b.range.len() < a.range.len()
            || b.data_range.len() < a.data_range.len()
            || b.functional.len() < a.functional.len()
            || b.empty.len() < a.empty.len()
            || b.linear.len() < a.linear.len()
            || b.inv.len() < a.inv.len()
            || a.clash.len() != b.clash.len()
            || old.selfs.len() != self.selfs.len()
            || super::saturate::plain(old) != super::saturate::plain(self)
            || !old.universal.is_empty()
            || !self.universal.is_empty()
            || !old.keys.is_empty()
            || !self.keys.is_empty()
            || !old.nominals.is_empty()
            || !self.nominals.is_empty()
            || old.nominal_selfs
            || self.nominal_selfs
            || old.tops != self.tops
            || self.tops[1].is_some()
            || old.horn.as_ref().map(|h| h.dominate) != self.horn.as_ref().map(|h| h.dominate)
            || old.dt.is_some() != self.dt.is_some()
        {
            return Some(false);
        }
        match (&old.dl, &self.dl) {
            (None, None) => {}
            (Some(da), Some(db)) if da.keyed_by_expressions() && db.keyed_by_expressions() => {}
            _ => return Some(false),
        }
        let mut ticks = 0usize;
        for (sa, sb) in a.sup.iter().zip(&b.sup[..n]) {
            if tick_changed(&mut ticks, stop) {
                return None;
            }
            if !slice_eq_poll(sa, sb, &mut ticks, stop)? {
                return Some(false);
            }
        }
        for (x, y) in [(&a.comp, &b.comp), (&b.comp, &a.comp)] {
            for (k, v) in x {
                if tick_changed(&mut ticks, stop) {
                    return None;
                }
                if (k.0 as usize) < n && (k.1 as usize) < n {
                    let Some(yv) = y.get(k) else {
                        return Some(false);
                    };
                    if !slice_eq_poll(v, yv, &mut ticks, stop)? {
                        return Some(false);
                    }
                }
            }
        }
        if !slice_eq_poll(&a.right, &b.right[..a.right.len()], &mut ticks, stop)?
            || !slice_eq_poll(&a.left, &b.left[..a.left.len()], &mut ticks, stop)?
            || !slice_eq_poll(&a.reflexive, &b.reflexive, &mut ticks, stop)?
            || !slice_eq_poll(&a.range, &b.range[..a.range.len()], &mut ticks, stop)?
        {
            return Some(false);
        }
        for (va, vb) in a.data_range.iter().zip(&b.data_range[..a.data_range.len()]) {
            if tick_changed(&mut ticks, stop) {
                return None;
            }
            let same = match (va, vb) {
                (None, None) => true,
                (Some(va), Some(vb)) => old.vals.get(*va as usize) == self.vals.get(*vb as usize),
                _ => false,
            };
            if !same {
                return Some(false);
            }
        }
        for (fa, fb) in a.functional.iter().zip(&b.functional[..a.functional.len()]) {
            if tick_changed(&mut ticks, stop) {
                return None;
            }
            if !slice_eq_poll(fa, fb, &mut ticks, stop)? {
                return Some(false);
            }
        }
        if !slice_eq_poll(&a.empty, &b.empty[..a.empty.len()], &mut ticks, stop)?
            || !slice_eq_poll(&a.linear, &b.linear[..a.linear.len()], &mut ticks, stop)?
            || !slice_eq_poll(&a.inv, &b.inv[..a.inv.len()], &mut ticks, stop)?
            || !slice_eq_poll(&a.clash, &b.clash, &mut ticks, stop)?
            || !slice_eq_poll(&old.selfs, &self.selfs, &mut ticks, stop)?
        {
            return Some(false);
        }
        if let (Some(a_dt), Some(b_dt)) = (&old.dt, &self.dt)
            && !b_dt.modes_compatible_with_stop(a_dt, n, stop)?
        {
            return Some(false);
        }
        Some(true)
    }

    fn disjoint_set_count<E>(&self, poll: &impl Fn() -> Result<(), E>) -> Result<usize, E> {
        let mut max_set = None;
        for memberships in self.disjoint.chunks(POLL_CHANGED) {
            poll()?;
            max_set = max_set.max(memberships.iter().copied().max());
        }
        Ok(max_set.map_or(0, |s| s as usize + 1))
    }

    /// For each expression of `old`, whether a context that holds it reads other rules
    /// here than a run over `old` gave it (roadmap item 753): another successor, told
    /// superexpressions, conjunctions, negative existential restrictions or
    /// disjointness sets, as sets, or other flags of the Horn rules, on the expression
    /// or, for a universal restriction, on its filler, changed EL data ranges or
    /// negative data restrictions, changed datatype-checker components or refire
    /// triggers, and in a run of Tier D other clauses with the expression in their body
    /// or another hypothetical successor. `poll` can stop disjointness numbering and
    /// the scan of the old expressions.
    #[allow(dead_code)]
    pub fn changed<E>(
        &self,
        old: &Index,
        poll: &impl Fn() -> Result<(), E>,
    ) -> Result<Vec<bool>, E> {
        self.changed_with_dt(old, None, poll)
    }

    /// [`Index::changed`], incorporating pre-aligned [`DtIx`](super::dt_sat::DtIx)
    /// component changes when datatype-checker components are active.
    pub fn changed_with_dt<E>(
        &self,
        old: &Index,
        dt_align: Option<&super::dt_sat::DtAlign>,
        poll: &impl Fn() -> Result<(), E>,
    ) -> Result<Vec<bool>, E> {
        let n = old.heads.len();
        debug_assert!(self.heads.len() >= n);
        poll()?;
        // Scope the disjointness numbering table so it drops before `changed` is allocated.
        let (old_sets, new_sets) = if old.disjoint.is_empty() && self.disjoint.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            let old_n = old.disjoint_set_count(poll)?;
            let new_n = self.disjoint_set_count(poll)?;
            // The disjointness sets of each index by their members, pre-sized to avoid
            // amortized doubling during the comparison.
            let members = |ix: &Index, n_sets: usize| -> Result<Vec<Vec<Ex>>, E> {
                if n_sets == 0 {
                    return Ok(Vec::new());
                }
                let mut counts = vec![0u32; n_sets];
                for memberships in ix.disjoint.chunks(POLL_CHANGED) {
                    poll()?;
                    for &s in memberships {
                        counts[s as usize] += 1;
                    }
                }
                let mut sets: Vec<Vec<Ex>> = Vec::with_capacity(n_sets);
                for counts in counts.chunks(POLL_CHANGED) {
                    poll()?;
                    sets.extend(counts.iter().map(|&c| Vec::with_capacity(c as usize)));
                }
                drop(counts);
                let mut memberships = 0usize;
                for (e, h) in ix.heads.iter().enumerate() {
                    if e.is_multiple_of(POLL_CHANGED) {
                        poll()?;
                    }
                    for &s in ix.disjoint_of(h) {
                        sets[s as usize].push(e as Ex);
                        memberships += 1;
                        if memberships.is_multiple_of(POLL_CHANGED) {
                            poll()?;
                        }
                    }
                }
                Ok(sets)
            };
            // Each set has one number in both indexes, so that an expression's sets compare
            // without their members.
            let mut numbers: FxHashMap<Vec<Ex>, u32> = FxHashMap::with_capacity_and_hasher(
                old_n.saturating_add(new_n),
                Default::default(),
            );
            let mut numbered = |sets: Vec<Vec<Ex>>| -> Result<Vec<u32>, E> {
                let mut out = Vec::with_capacity(sets.len());
                for (i, members) in sets.into_iter().enumerate() {
                    if i.is_multiple_of(POLL_CHANGED) {
                        poll()?;
                    }
                    let next = numbers.len() as u32;
                    out.push(*numbers.entry(members).or_insert(next));
                }
                Ok(out)
            };
            let old_sets = numbered(members(old, old_n)?)?;
            let new_sets = numbered(members(self, new_n)?)?;
            (old_sets, new_sets)
        };
        let flag = |ix: &Index, e: usize| -> u8 {
            ix.horn
                .as_ref()
                .and_then(|h| h.flags.get(e).copied())
                .unwrap_or(0)
        };
        // Data roles of `old` whose negative data restrictions changed (as sets of
        // `(DataVal, Ex)` pairs). Any `DataSome(r, _)` with a superrole in this set
        // must be marked changed so functional data groups re-run `data_matches`.
        // Each `neg_data` list is canonicalized during `Index::build`, so comparison
        // is a zero-allocation linear scan with cooperative stop polling, and both
        // role sets are pre-sized to avoid transient rehash memory.
        let mut changed_neg_roles: FxHashSet<Role> = FxHashSet::default();
        if !old.neg_data.is_empty() || !self.neg_data.is_empty() {
            let old_roles = old.roles.sup.len();
            let max_roles = (old.neg_data.len() + self.neg_data.len()).min(old_roles);
            let mut seen_roles: FxHashSet<Role> =
                FxHashSet::with_capacity_and_hasher(max_roles, Default::default());
            changed_neg_roles = FxHashSet::with_capacity_and_hasher(max_roles, Default::default());
            #[cfg(test)]
            let (seen_init_cap, changed_init_cap) =
                (seen_roles.capacity(), changed_neg_roles.capacity());
            let mut ticks = 0usize;
            for &r in old.neg_data.keys().chain(self.neg_data.keys()) {
                ticks += 1;
                if ticks >= POLL_CHANGED {
                    ticks = 0;
                    poll()?;
                }
                if (r as usize) >= old_roles || !seen_roles.insert(r) {
                    continue;
                }
                let old_v = old.neg_data.get(&r).map_or(&[][..], Vec::as_slice);
                let new_v = self.neg_data.get(&r).map_or(&[][..], Vec::as_slice);
                let mut same_neg = old_v.len() == new_v.len();
                if same_neg {
                    for (&(wa, xa), &(wb, xb)) in old_v.iter().zip(new_v) {
                        ticks += 1;
                        if ticks >= POLL_CHANGED {
                            ticks = 0;
                            poll()?;
                        }
                        if xa != xb || old.vals[wa as usize] != self.vals[wb as usize] {
                            same_neg = false;
                            break;
                        }
                    }
                }
                if !same_neg {
                    changed_neg_roles.insert(r);
                }
            }
            #[cfg(test)]
            neg_role_set_record(
                seen_init_cap,
                seen_roles.capacity(),
                changed_init_cap,
                changed_neg_roles.capacity(),
            );
        }
        let mut changed = vec![false; n];
        for (e, changed) in changed.iter_mut().enumerate() {
            if e.is_multiple_of(POLL_CHANGED) {
                poll()?;
            }
            let (a, b) = (&old.heads[e], &self.heads[e]);
            debug_assert_eq!(a.node, b.node);
            debug_assert!(old.told_of(a).is_sorted() && self.told_of(b).is_sorted());
            debug_assert!(old.conj_of(a).is_sorted() && self.conj_of(b).is_sorted());
            debug_assert!(old.exist_of(a).is_sorted() && self.exist_of(b).is_sorted());
            let sets = |ix: &Index, h: &Head, sets: &[u32]| -> Vec<u32> {
                let mut v: Vec<u32> = ix
                    .disjoint_of(h)
                    .iter()
                    .map(|&s| sets[s as usize])
                    .collect();
                v.sort_unstable();
                v
            };
            let filler = match a.node {
                Node::Forall(_, f) => flag(old, f as usize) != flag(self, f as usize),
                _ => false,
            };
            let succ_changed = match a.node {
                Node::DataSome(..) => {
                    a.succ != b.succ
                        && old.vals.get(a.succ as usize) != self.vals.get(b.succ as usize)
                }
                _ => a.succ != b.succ,
            };
            let data_changed =
                match a.node {
                    Node::DataSome(r, _) => {
                        !changed_neg_roles.is_empty()
                            && old.roles.sup.get(r as usize).is_some_and(|sup| {
                                sup.iter().any(|g| changed_neg_roles.contains(g))
                            })
                    }
                    Node::DtAtom(id) => match (&old.dt, dt_align) {
                        (Some(old_dt), Some(align)) => old_dt
                            .keys
                            .get(id as usize)
                            .and_then(|k| old_dt.roles.get(&k.role))
                            .and_then(|ri| align.changed_comps.get(ri.comp as usize).copied())
                            .unwrap_or(true),
                        _ => false,
                    },
                    _ => false,
                };
            *changed = a.node != b.node
                || succ_changed
                || data_changed
                || filler
                || flag(old, e) != flag(self, e)
                || old.told_of(a) != self.told_of(b)
                || old.conj_of(a) != self.conj_of(b)
                || old.exist_of(a) != self.exist_of(b)
                || a.disj.len != b.disj.len
                || (a.disj.len != 0 && sets(old, a, &old_sets) != sets(self, b, &new_sets));
        }
        if let (Some(old_dt), Some(new_dt), Some(align)) = (&old.dt, &self.dt, dt_align) {
            let mut ticks = 0usize;
            for e in 0..n {
                ticks += 1;
                if ticks >= POLL_CHANGED {
                    ticks = 0;
                    poll()?;
                }
                if let Node::DtFire(t) = old.heads[e].node {
                    let refire_same = match (old_dt.refire_of(t), new_dt.refire_of(t)) {
                        (None, None) => true,
                        (Some(old_cs), Some(new_cs)) if old_cs.len() == new_cs.len() => {
                            let mut same = true;
                            for &oc in old_cs {
                                ticks += 1;
                                if ticks >= POLL_CHANGED {
                                    ticks = 0;
                                    poll()?;
                                }
                                let mapped = align.comp_map.get(oc as usize).copied().flatten();
                                if !mapped.is_some_and(|nc| new_cs.binary_search(&nc).is_ok()) {
                                    same = false;
                                    break;
                                }
                            }
                            same
                        }
                        _ => false,
                    };
                    if !refire_same {
                        changed[e] = true;
                        if (t as usize) < n {
                            changed[t as usize] = true;
                        }
                    }
                }
            }
        }
        drop(old_sets);
        drop(new_sets);
        poll()?;
        if let (Some(a), Some(b)) = (&old.dl, &self.dl) {
            b.changed(a, &mut changed);
        }
        Ok(changed)
    }

    /// The bytes of the `Vec<bool>` returned by [`Index::changed`] for this `old` index.
    pub fn changed_bytes(&self) -> usize {
        self.heads.len()
    }

    /// Upper bound on peak scratch bytes that [`Index::changed_with_dt`] and
    /// [`DtIx::align`](super::dt_sat::DtIx::align) allocate for the comparison with
    /// `old`: disjointness set numbering (dropped before `changed`), the `changed` mark
    /// vector with per-head disjointness comparison and negative-data role tables
    /// (dropped before Tier D), datatype-checker alignment tables, and the `changed` mark
    /// vector with Tier D's clause-key sets. Per-head `told`, `conj` and `exist` slices
    /// are pre-sorted in [`Index::build`] and compared without allocation.
    pub fn changed_scratch<E>(
        &self,
        old: &Index,
        poll: &impl Fn() -> Result<(), E>,
    ) -> Result<usize, E> {
        let neg_roles = (old.neg_data.len() + self.neg_data.len()).min(old.roles.sup.len());
        let neg_tables = if neg_roles > 0 {
            super::dtcheck::range::map_buckets_for(neg_roles)
                .saturating_mul((std::mem::size_of::<Role>() + 1) * 2)
                .saturating_add(64)
        } else {
            0
        };
        let dt_scratch = match (&old.dt, &self.dt) {
            (Some(a), Some(b)) => b.align_scratch(a),
            _ => 0,
        };
        let old_sets = old.disjoint_set_count(poll)?;
        let new_sets = self.disjoint_set_count(poll)?;
        let sets = old_sets.saturating_add(new_sets);
        let memberships = old.disjoint.len().saturating_add(self.disjoint.len());
        let numbering_bytes = sets.saturating_mul(std::mem::size_of::<u32>());
        let disj_phase =
            super::dl_clause::hash_table_bytes(sets, std::mem::size_of::<(Vec<Ex>, u32)>())
                .saturating_add(memberships.saturating_mul(std::mem::size_of::<Ex>()))
                .saturating_add(numbering_bytes)
                .saturating_add(
                    old_sets
                        .max(new_sets)
                        .saturating_mul(std::mem::size_of::<Vec<Ex>>()),
                )
                .saturating_add(dt_scratch);
        // Only equal-length spans can allocate both comparison buffers.
        let mut max_span = 0;
        if old_sets != 0 && new_sets != 0 {
            let chunks = old
                .heads
                .chunks(POLL_CHANGED)
                .zip(self.heads.chunks(POLL_CHANGED));
            #[cfg(test)]
            let chunks = chunks
                .enumerate()
                .inspect(|&(i, _)| {
                    super::admission_test::hit(
                        super::admission_test::Point::CompareScratchHeadChunk(i),
                    );
                })
                .map(|(_, pair)| pair);
            for (old_heads, new_heads) in chunks {
                poll()?;
                for (a, b) in old_heads.iter().zip(new_heads) {
                    if a.disj.len == b.disj.len {
                        max_span = max_span.max(a.disj.len as usize);
                    }
                }
            }
        }
        let scan_phase = old
            .heads
            .len()
            .saturating_add(numbering_bytes)
            .saturating_add(max_span.saturating_mul(2 * std::mem::size_of::<u32>()))
            .saturating_add(neg_tables)
            .saturating_add(dt_scratch);
        let dl_phase =
            old.heads
                .len()
                .saturating_add(dt_scratch)
                .saturating_add(match (&old.dl, &self.dl) {
                    (Some(a), Some(b)) => b.changed_scratch(a, poll)?,
                    _ => 0,
                });
        poll()?;
        Ok(disj_phase.max(scan_phase).max(dl_phase))
    }

    /// The bytes the index takes, roughly, the role table included.
    pub fn bytes(&self) -> usize {
        self.bytes_poll(&mut |_| Ok::<(), std::convert::Infallible>(()))
            .unwrap_or_else(|e| match e {})
    }

    /// Like [`Index::bytes`], polling `tick` across variable-length tables and the Tier D index.
    pub fn bytes_poll<E>(&self, tick: &mut impl FnMut(usize) -> Result<(), E>) -> Result<usize, E> {
        let mut total = self.top_chain.as_ref().map_or(0, |rules| rules.bytes())
            + self.heads.capacity() * std::mem::size_of::<Head>()
            + self.told.capacity() * 4
            + self.conj.capacity() * 8
            + self.exist.capacity() * 8
            + self.disjoint.capacity() * 4
            + self.selfs.capacity() * 8
            + self.vals.capacity() * std::mem::size_of::<DataVal>()
            + self.universal.capacity() * 8
            + self.nominals.capacity() * 4
            + self.anonymous.capacity() * 4
            + self.horn.as_ref().map_or(0, |h| h.bytes())
            + self.dt.as_ref().map_or(0, |d| d.bytes());
        for v in &self.vals {
            tick(1)?;
            total += v.owned_bytes();
        }
        for v in self.neg_data.values() {
            tick(1)?;
            total += 48 + v.capacity() * 8;
        }
        total += self.roles.bytes_poll(tick)?;
        if let Some(d) = &self.dl {
            total += d.bytes_poll(tick)?;
        }
        Ok(total)
    }

    /// Logical input-size measure of the index, independent of allocator capacities.
    pub fn logical_bytes(&self) -> usize {
        self.heads.len() * std::mem::size_of::<Head>()
            + self.told.len() * 4
            + self.conj.len() * 8
            + self.exist.len() * 8
            + self.disjoint.len() * 4
            + self.selfs.len() * 8
            + self.vals.len() * std::mem::size_of::<DataVal>()
            + self
                .vals
                .iter()
                .map(DataVal::logical_owned_bytes)
                .sum::<usize>()
            + self
                .neg_data
                .values()
                .map(|v| 48 + v.len() * 8)
                .sum::<usize>()
            + self.universal.len() * 8
            + self.nominals.len() * 4
            + self.anonymous.len() * 4
            + self.roles.logical_bytes()
            + self.horn.as_ref().map_or(0, |h| h.logical_bytes())
            + self.dl.as_ref().map_or(0, |d| d.logical_bytes())
            + self.dt.as_ref().map_or(0, |d| d.logical_bytes())
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
            b.negative_as(a, it, true);
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
        // the clauses of Tier D read their body atoms as the left sides of inclusions
        if let Some(d) = &ax.dl {
            for (body, _) in &d.clauses {
                for &e in body {
                    b.negative(e, it);
                }
            }
            for (guard, _, _, _, _) in &d.lazy {
                if let Some(g) = *guard {
                    b.negative(g, it);
                }
            }
        }
        for &(r, w, x) in &b.neg_data {
            ix.neg_data.entry(r).or_default().push((w, x));
        }
        ix.vals = it.copy_values()?;
        for ns in ix.neg_data.values_mut() {
            ns.sort_unstable_by(|&(w1, x1), &(w2, x2)| {
                ix.vals[w1 as usize]
                    .cmp(&ix.vals[w2 as usize])
                    .then_with(|| x1.cmp(&x2))
            });
            ns.dedup_by(|(w1, x1), (w2, x2)| {
                *x1 == *x2 && ix.vals[*w1 as usize] == ix.vals[*w2 as usize]
            });
        }
        ix.numeric_ledger = it.ranges.ledger.clone();
        // An individual has an instance in every model, so a told `{a} ⊑ ∃r.{b}`, which
        // a property assertion `r(a, b)` is, puts `b` in the range of `r`. The index
        // tells `b` the range and gives the restriction `b`'s own context as its
        // successor, where the context of `{b} ⊓ range` would derive `b` and repeat
        // everything `b` holds (roadmap item 1025). The same restriction under a class
        // gets that successor too, which holds the range by the told subsumption.
        for &(a, c) in &ax.subs {
            if let Node::Nominal(_) = it.nodes[a as usize]
                && let Node::Exist(r, f) = it.nodes[c as usize]
                && let Node::Nominal(_) = it.nodes[f as usize]
                && ix.roles.range.get(r as usize).copied().flatten().is_some()
            {
                b.asserted.insert(c);
            }
        }
        for &(a, c) in &ax.subs {
            b.positive(c, it, &mut ix);
            if a != c {
                told.push((a, c));
            }
        }
        if let Some(d) = &ax.dl {
            for (_, head) in &d.clauses {
                for &e in head {
                    b.positive(e, it, &mut ix);
                }
            }
            for (_, _, _, _, head) in &d.lazy {
                for &e in head {
                    b.positive(e, it, &mut ix);
                }
            }
        }
        // A loop by a role is a link from a context to itself, so the context takes the
        // role's range (saturate.rs, `Concl::Loop`). Loops come from self restrictions,
        // from chains that compose loops, from individuals linked to themselves and from
        // the Horn rules, by roles that need not occur in any positive existential, so
        // every range occurs positively (roadmap item 1262). A range that no context
        // takes still costs its index entries, such as the intersections of the fillers
        // of its existentials with their roles' ranges, their heads and successor
        // entries and the told subsumers of its data restrictions, but no context is
        // saturated for it.
        let ranges: Vec<Ex> = ix.roles.range.iter().flatten().copied().collect();
        for c in ranges {
            b.positive(c, it, &mut ix);
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
        // a universal restriction on a universal role, which the Horn rules of roadmap
        // item 692 can hold: its filler holds everywhere once a real context holds it
        if top.is_some() && ax.horn.is_some() {
            for (e, n) in it.nodes.iter().enumerate() {
                if let Node::Forall(r, a) = *n
                    && top.is_some_and(|t| ix.roles.sub(t, r))
                {
                    ix.universal.push((e as Ex, a));
                }
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
            it.ranges.check_halt()?;
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
        // the told subsumers of the checker's body atoms re-evaluate the atoms'
        // components, whichever of a subsumer and an atom a context gains first. The
        // markers are interned before the Horn index, whose conjunctions' contexts take
        // the ids above every expression
        let run = it.dt_run.take();
        let (refire, refire_guard) = match run.as_deref() {
            Some(r) => match dt_fires(it, r, &b, &mut told) {
                Ok(out) => out,
                Err(e) => {
                    it.dt_run = run;
                    return Err(e);
                }
            },
            None => (RefireMap::default(), None),
        };
        it.dt_run = run;
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
        ix.conj = group_sorted(&b.conj, |h| &mut h.conj, &mut ix.heads);
        ix.exist = group_sorted(&b.exist, |h| &mut h.exist, &mut ix.heads);
        ix.disjoint = group(&disj, |h| &mut h.disj, &mut ix.heads);
        ix.nominal_selfs = !b.selfs.is_empty() && !it.individuals.is_empty();
        ix.selfs = std::mem::take(&mut b.selfs);
        ix.selfs.sort_unstable();
        ix.keys = ax.keys.clone();
        ix.nominals = it.nominals().collect();
        ix.anonymous = it.anonymous_nominals();
        ix.vals = it.copy_values()?;
        ix.numeric_ledger = it.ranges.ledger.clone();
        if let Some(d) = &ax.dl {
            let dx = super::dl_rules::DlIx::new(d, &ax.disjoint, &ix, it.nodes.len());
            ix.dl = Some(Box::new(dx));
        }
        // the Horn rules pass over dominated successors when no context's conclusions
        // depend on its predecessors (item 710)
        let plain = super::saturate::plain(&ix);
        if let Some(h) = ix.horn.as_mut() {
            h.dominate = plain;
        }
        if ax.dl.is_none()
            && let Some(run) = it.dt_run.as_deref()
        {
            ix.dt = Some(Box::new(dt_index(
                ax,
                it,
                run,
                &ix,
                &b,
                refire,
                refire_guard,
            )?));
        }
        Ok(ix)
    }
}

/// What the checker's rule reads: the roles of the checker components with their
/// superroles, ranges and functionality, and the body atoms with their told subsumers,
/// from the atoms this run's axioms reach, never from the interner's whole table.
fn dt_index(
    ax: &Axioms,
    it: &Interner,
    run: &DtRun,
    ix: &Index,
    b: &Builder,
    refire: RefireMap,
    refire_guard: Option<super::numeric::ScratchGuard>,
) -> Result<super::dt_sat::DtIx, Halt> {
    use super::dt_sat::{Body, DtIx, RoleIx};
    use super::dtcheck::range::map_buckets_for;
    let checker_comp = |r: Role| -> Option<u32> {
        match it.roles.get(r as usize) {
            Some(RoleName::Data(d)) if run.comps.checker(d) => run.comps.component(d),
            _ => None,
        }
    };
    let mut n_checker_roles = 0usize;
    let mut exact_supers = 0usize;
    for r in 0..it.roles.len() {
        let role = r as Role;
        if let Some(comp) = checker_comp(role) {
            n_checker_roles += 1;
            if let Some(sup) = ix.roles.sup.get(r) {
                exact_supers = exact_supers.saturating_add(
                    sup.iter()
                        .filter(|&&s| s != role && checker_comp(s) == Some(comp))
                        .count(),
                );
            }
        }
    }
    let exact_ranges = ax
        .dt_ranges
        .iter()
        .filter(|&&(r, _)| checker_comp(r).is_some())
        .count();
    let exact_disjoint = ax
        .dt_disjoint
        .iter()
        .filter(|&&(a, b)| checker_comp(a).is_some() && checker_comp(b).is_some())
        .count();
    let roles_backing = if n_checker_roles == 0 {
        0
    } else {
        map_buckets_for(n_checker_roles)
            .saturating_mul(std::mem::size_of::<(Role, RoleIx)>() + 1)
            .saturating_add(16)
    };
    // Pushing `k` items into `Vec::new()` reaches capacity <= 4*k (including min-4 for k=1)
    // and peak growth <= 4*k for every k >= 0; `supers` is preallocated to exact length.
    let roles_est = roles_backing
        .saturating_add(n_checker_roles.saturating_mul(std::mem::size_of::<Role>()))
        .saturating_add(exact_supers.saturating_mul(std::mem::size_of::<Role>()))
        .saturating_add(
            exact_ranges
                .saturating_mul(4)
                .saturating_mul(std::mem::size_of::<super::dtcheck::range::Rid>()),
        );
    let told_bytes: usize = b
        .dt_body
        .iter()
        .map(|&e| ix.told_of(ix.head(e)).len().saturating_mul(4))
        .sum();
    let ncomps = run.comps.len();
    let body_est = ncomps
        .saturating_mul(std::mem::size_of::<Vec<Body>>() + 1)
        .saturating_add(
            b.dt_body
                .len()
                .saturating_mul(4)
                .saturating_mul(std::mem::size_of::<Body>()),
        )
        .saturating_add(told_bytes);
    let disjoint_est = exact_disjoint.saturating_mul(std::mem::size_of::<(Role, Role)>());
    let clone_est = std::mem::size_of::<DtIx>()
        .saturating_add(
            it.dt
                .atoms
                .len()
                .saturating_mul(std::mem::size_of::<DtKey>()),
        )
        .saturating_add(
            it.dt
                .ranges()
                .map_or_else(super::dtcheck::range::Ranges::default_bytes, |r| r.bytes()),
        )
        .saturating_add(run.comps.bytes());
    let total_est = roles_est
        .saturating_add(body_est)
        .saturating_add(disjoint_est)
        .saturating_add(clone_est)
        .saturating_add(refire.bytes())
        .saturating_add(256);
    let mut guard = refire_guard.or_else(|| it.ranges.budget.as_ref().map(|b| b.scratch_guard()));
    if let Some(g) = guard.as_mut() {
        let extra = total_est.saturating_sub(g.bytes());
        g.grow(extra).map_err(|e| match e {
            super::Error::Memory { used, .. } => Halt::Memory(used),
            _ => Halt::Stopped,
        })?;
    }
    let mut roles: FxHashMap<Role, RoleIx> =
        FxHashMap::with_capacity_and_hasher(n_checker_roles, Default::default());
    for (r, name) in it.roles.iter().enumerate() {
        if let RoleName::Data(d) = name
            && run.comps.checker(d)
            && let Some(comp) = run.comps.component(d)
        {
            roles.insert(
                r as Role,
                RoleIx {
                    comp,
                    supers: Vec::new(),
                    ranges: Vec::new(),
                    functional: false,
                },
            );
        }
    }
    let ids: Vec<Role> = roles.keys().copied().collect();
    for r in ids {
        let comp = roles[&r].comp;
        let sup_slice = ix
            .roles
            .sup
            .get(r as usize)
            .map_or(&[][..], |v| v.as_slice());
        let nsup = sup_slice
            .iter()
            .filter(|&&s| s != r && roles.get(&s).is_some_and(|x| x.comp == comp))
            .count();
        let mut supers = Vec::with_capacity(nsup);
        for &s in sup_slice {
            if s != r && roles.get(&s).is_some_and(|x| x.comp == comp) {
                supers.push(s);
            }
        }
        if let Some(x) = roles.get_mut(&r) {
            x.supers = supers;
        }
    }
    for &(r, x) in &ax.dt_ranges {
        if let Some(ri) = roles.get_mut(&r) {
            ri.ranges.push(x);
        }
    }
    for &r in &ax.functional {
        if let Some(ri) = roles.get_mut(&r) {
            ri.functional = true;
        }
    }
    let mut body: Vec<Vec<Body>> = (0..ncomps).map(|_| Vec::new()).collect();
    for &e in &b.dt_body {
        let Node::DtAtom(id) = it.nodes[e as usize] else {
            continue;
        };
        let key = it.dt.atoms[id as usize];
        let Some(comp) = roles.get(&key.role).map(|r| r.comp) else {
            continue;
        };
        // an atom the Horn rules also read as a filler has more than its told uses
        let other = b.dt_other.contains(&e)
            || ix
                .horn
                .as_ref()
                .is_some_and(|h| h.is(e, super::horn::HornIx::FILLER));
        // a told subsumer that has no marker for the atom's component, such as one the
        // Horn index tells after the markers are made, would make the rule's answer
        // depend on the order of the conclusions, so the atom has more than its told uses
        let told: Option<Box<[Ex]>> = (!other)
            .then(|| {
                let raw = ix.told_of(ix.head(e));
                let ntold = raw
                    .iter()
                    .filter(|&&t| !matches!(it.nodes[t as usize], Node::DtFire(_)))
                    .count();
                let mut v = Vec::with_capacity(ntold);
                for &t in raw {
                    if !matches!(it.nodes[t as usize], Node::DtFire(_)) {
                        v.push(t);
                    }
                }
                v.into_boxed_slice()
            })
            .filter(|ts| {
                ts.iter().all(|&t| {
                    t == TOP
                        || refire
                            .get(&t)
                            .is_some_and(|cs| cs.binary_search(&comp).is_ok())
                })
            });
        debug_assert!(
            key.at_least,
            "a body atom of the Horn tier is an at-least atom"
        );
        body[comp as usize].push(Body { e, told });
    }
    // Canonicalize body order by expression id so reordering unchanged axioms does not
    // alter `DtIx::align` or cache keys.
    for v in &mut body {
        v.sort_unstable_by_key(|b| b.e);
    }
    let mut disjoint: Vec<(Role, Role)> = Vec::with_capacity(exact_disjoint);
    for &(a, b) in &ax.dt_disjoint {
        if roles.contains_key(&a) && roles.contains_key(&b) {
            disjoint.push((a, b));
        }
    }
    disjoint.sort_unstable();
    disjoint.dedup();
    let dt_ix = DtIx::new(
        it.dt.atoms.clone(),
        roles,
        body,
        disjoint,
        it.dt.ranges().cloned().unwrap_or_default(),
        run.comps.clone(),
        refire,
    );
    let retained = dt_ix.bytes();
    debug_assert!(
        total_est >= retained,
        "dt_index upfront estimate {total_est} must cover retained DtIx::bytes() {retained}"
    );
    if let Some(g) = guard {
        g.commit_retained(retained).map_err(|e| match e {
            super::Error::Memory { used, .. } => Halt::Memory(used),
            _ => Halt::Stopped,
        })?;
    }
    Ok(dt_ix)
}

/// The component of a checker atom, by its role, when the run's axioms reach it.
fn dt_comp(it: &Interner, run: &DtRun, e: Ex) -> Option<u32> {
    let Node::DtAtom(id) = it.nodes[e as usize] else {
        return None;
    };
    let role = it.dt.atoms[id as usize].role;
    match &it.roles[role as usize] {
        RoleName::Data(d) if run.comps.checker(d) => run.comps.component(d),
        _ => None,
    }
}

/// The low bits record whether the vector is sorted and whether it has resumed after
/// a deferred growth. A target indexes `Interner::nodes`, whose entries are at least
/// two bytes and whose backing is bounded by `isize::MAX` bytes.
struct RefireEntry {
    target_and_flags: usize,
    components: Vec<u32>,
}

const _: () = assert!(std::mem::size_of::<RefireEntry>() == std::mem::size_of::<(Ex, Vec<u32>)>());
const _: () = assert!(std::mem::size_of::<Node>() >= 2);

impl RefireEntry {
    const SORTED: usize = 1;
    const RESUMED: usize = 2;

    fn target(&self) -> Ex {
        (self.target_and_flags >> 2) as Ex
    }

    fn sorted(&self) -> bool {
        self.target_and_flags & Self::SORTED != 0
    }

    fn resumed(&self) -> bool {
        self.target_and_flags & Self::RESUMED != 0
    }

    fn set_sorted(&mut self) {
        self.target_and_flags |= Self::SORTED;
    }

    fn resume(&mut self) {
        self.target_and_flags = (self.target_and_flags & !Self::SORTED) | Self::RESUMED;
    }
}

#[derive(Default)]
pub(super) struct RefireMap {
    table: hashbrown::HashTable<RefireEntry>,
}

const _: () =
    assert!(std::mem::size_of::<RefireMap>() == std::mem::size_of::<FxHashMap<Ex, Vec<u32>>>());

impl RefireMap {
    #[inline]
    fn hash(target: Ex) -> u64 {
        use std::hash::BuildHasher;
        rustc_hash::FxBuildHasher.hash_one(target)
    }

    pub(super) fn get(&self, target: &Ex) -> Option<&Vec<u32>> {
        self.table
            .find(Self::hash(*target), |entry| entry.target() == *target)
            .map(|entry| &entry.components)
    }

    /// Inserts an absent target that indexes the interner's node vector.
    pub(super) fn insert(&mut self, target: Ex, components: Vec<u32>) {
        debug_assert!(target as usize <= usize::MAX >> 2);
        self.table.insert_unique(
            Self::hash(target),
            RefireEntry {
                target_and_flags: (target as usize) << 2,
                components,
            },
            |entry| Self::hash(entry.target()),
        );
    }

    fn table_bytes(&self) -> usize {
        super::dtcheck::range::table_backing_bytes::<RefireEntry>(self.table.capacity())
    }

    pub(super) fn bytes(&self) -> usize {
        self.table_bytes().saturating_add(
            self.table
                .iter()
                .map(|entry| {
                    entry
                        .components
                        .capacity()
                        .saturating_mul(std::mem::size_of::<u32>())
                })
                .sum::<usize>(),
        )
    }

    pub(super) fn logical_bytes(&self) -> usize {
        self.table
            .len()
            .saturating_mul(std::mem::size_of::<RefireEntry>())
            .saturating_add(
                self.table
                    .iter()
                    .map(|entry| {
                        entry
                            .components
                            .len()
                            .saturating_mul(std::mem::size_of::<u32>())
                    })
                    .sum::<usize>(),
            )
    }
}

/// Doubles a refire component vector's capacity through `refire_guard`, admitting the full
/// destination allocation while the old backing is still charged and releasing the old
/// backing charge immediately afterward.
fn dt_fires_grow_vec(
    cs: &mut Vec<u32>,
    refire_guard: &mut Option<super::numeric::ScratchGuard>,
    map_charged: usize,
    vec_charged: &mut usize,
) -> Result<(), Halt> {
    let old_bytes = cs.capacity().saturating_mul(std::mem::size_of::<u32>());
    let next_cap = cs.capacity().saturating_mul(2).max(4);
    let next_bytes = next_cap.saturating_mul(std::mem::size_of::<u32>());
    if let Some(g) = refire_guard.as_mut() {
        g.grow(next_bytes).map_err(dt_fires_halt)?;
    }
    cs.reserve_exact(next_cap - cs.len());
    let new_bytes = cs.capacity().saturating_mul(std::mem::size_of::<u32>());
    *vec_charged = vec_charged
        .saturating_sub(old_bytes)
        .saturating_add(new_bytes);
    if let Some(g) = refire_guard.as_mut() {
        let target = map_charged.saturating_add(*vec_charged);
        if g.bytes() > target {
            g.shrink(g.bytes() - target);
        } else if target > g.bytes() {
            g.grow(target - g.bytes()).map_err(dt_fires_halt)?;
        }
    }
    Ok(())
}

type RefireScratch = (RefireMap, Option<super::numeric::ScratchGuard>);

enum RefireGrowth {
    Deferred { skipped: bool },
    Eager,
}

/// Tells each told subsumer of a body atom whose occurrences are all told its
/// [`Node::DtFire`] marker, and gives for each subsumer the components it re-evaluates
/// alongside the live scratch reservation covering the returned `refire` map.
fn dt_fires(
    it: &mut Interner,
    run: &DtRun,
    b: &Builder,
    told: &mut Vec<(Ex, Ex)>,
) -> Result<RefireScratch, Halt> {
    let mut growth = RefireGrowth::Deferred { skipped: false };
    match dt_fires_attempt(it, run, b, told, &mut growth) {
        Ok(Some(result)) => return Ok(result),
        Ok(None) => {}
        // Deferral can reorder overlapping allocations across targets.
        Err(Halt::Memory(_)) if matches!(growth, RefireGrowth::Deferred { skipped: true }) => {}
        Err(error) => return Err(error),
    }
    // The first scan dropped its scratch before publishing markers. Replay in the
    // original growth order under the same budget and its already paid peak.
    if let Some(budget) = &it.ranges.budget {
        budget.poll().map_err(dt_fires_halt)?;
    }
    Ok(
        dt_fires_attempt(it, run, b, told, &mut RefireGrowth::Eager)?
            .expect("eager scan completes without replay"),
    )
}

fn dt_fires_halt(error: super::Error) -> Halt {
    match error {
        super::Error::Memory { used, .. } => Halt::Memory(used),
        _ => Halt::Stopped,
    }
}

fn dt_fires_attempt(
    it: &mut Interner,
    run: &DtRun,
    b: &Builder,
    told: &mut Vec<(Ex, Ex)>,
    growth: &mut RefireGrowth,
) -> Result<Option<RefireScratch>, Halt> {
    use super::dtcheck::range::table_growth;
    let n_eligible = b
        .dt_body
        .iter()
        .filter(|&&e| !b.dt_other.contains(&e) && dt_comp(it, run, e).is_some())
        .count();
    if n_eligible == 0 {
        return Ok(Some((RefireMap::default(), None)));
    }
    let mut eligible_guard = it.ranges.budget.as_ref().map(|b| b.scratch_guard());
    if let Some(g) = eligible_guard.as_mut() {
        g.grow(n_eligible.saturating_mul(std::mem::size_of::<(Ex, u32)>()))
            .map_err(dt_fires_halt)?;
    }
    let mut eligible: Vec<(Ex, u32)> = Vec::with_capacity(n_eligible);
    for &e in &b.dt_body {
        if b.dt_other.contains(&e) {
            continue;
        }
        if let Some(comp) = dt_comp(it, run, e) {
            eligible.push((e, comp));
        }
    }
    eligible.sort_unstable_by_key(|&(e, _)| e);
    eligible.dedup();
    let mut refire_guard = it.ranges.budget.as_ref().map(|b| b.scratch_guard());
    let mut map_charged = 0usize;
    let mut vec_charged = 0usize;
    let mut refire = RefireMap::default();
    for &(a, c) in told.iter().chain(&b.told) {
        if c == TOP || !matches!(it.nodes[a as usize], Node::DtAtom(_)) {
            continue;
        }
        let Ok(pos) = eligible.binary_search_by_key(&a, |&(e, _)| e) else {
            continue;
        };
        let comp = eligible[pos].1;
        let hash = RefireMap::hash(c);
        let matches_target = |entry: &RefireEntry| entry.target() == c;
        if refire.table.find(hash, matches_target).is_none() {
            let table_growth =
                table_growth::<RefireEntry>(refire.table.len(), refire.table.capacity());
            if let Some(g) = refire_guard.as_mut() {
                g.grow(table_growth).map_err(dt_fires_halt)?;
            }
            refire.insert(c, Vec::new());
            let new_map_bytes = refire.table_bytes();
            if let Some(g) = refire_guard.as_mut() {
                let target = vec_charged.saturating_add(new_map_bytes);
                if g.bytes() > target {
                    g.shrink(g.bytes() - target);
                } else if target > g.bytes() {
                    g.grow(target - g.bytes()).map_err(dt_fires_halt)?;
                }
            }
            map_charged = new_map_bytes;
        }
        let entry = refire
            .table
            .find_mut(hash, matches_target)
            .expect("inserted above");
        let sorted = entry.sorted();
        let resumed = entry.resumed();
        let cs = &mut entry.components;
        if cs.last() == Some(&comp) {
            continue;
        }
        if sorted {
            if cs.binary_search(&comp).is_ok() {
                continue;
            }
            // The first deferred growth is one the eager scan already took. After
            // resuming, grow only when the new distinct component cannot fit at all.
            if resumed && cs.len() < cs.capacity() {
                return Ok(None);
            }
            dt_fires_grow_vec(cs, &mut refire_guard, map_charged, &mut vec_charged)?;
            cs.push(comp);
            entry.resume();
            continue;
        }
        if cs.len() == cs.capacity() {
            if !cs.is_empty() {
                #[cfg(test)]
                dt_fires_record_compaction(cs.len());
                cs.sort_unstable();
                cs.dedup();
            }
            let dense = cs.len() == cs.capacity()
                || cs.len().saturating_mul(4) > cs.capacity().saturating_mul(3);
            let duplicate = cs.binary_search(&comp).is_ok();
            if dense
                && duplicate
                && let RefireGrowth::Deferred { skipped } = growth
            {
                entry.set_sorted();
                *skipped = true;
                continue;
            }
            if dense {
                // Suppressed duplicates changed the compaction points. A near-full
                // vector could grow here and reach dt_index larger than in eager order.
                if resumed && cs.len() < cs.capacity() {
                    return Ok(None);
                }
                dt_fires_grow_vec(cs, &mut refire_guard, map_charged, &mut vec_charged)?;
            }
            if duplicate {
                continue;
            }
        }
        cs.push(comp);
    }
    drop(eligible);
    drop(eligible_guard);
    for entry in &mut refire.table {
        if !entry.sorted() {
            entry.components.sort_unstable();
            entry.components.dedup();
        }
    }
    let mut ts_guard = it.ranges.budget.as_ref().map(|b| b.scratch_guard());
    if let Some(g) = ts_guard.as_mut() {
        g.grow(refire.table.len().saturating_mul(std::mem::size_of::<Ex>()))
            .map_err(dt_fires_halt)?;
    }
    let mut ts: Vec<Ex> = Vec::with_capacity(refire.table.len());
    ts.extend(refire.table.iter().map(RefireEntry::target));
    ts.sort_unstable();
    for t in ts {
        let f = it.node(Node::DtFire(t));
        told.push((t, f));
    }
    drop(ts_guard);
    #[cfg(debug_assertions)]
    if let Some(g) = refire_guard.as_ref() {
        debug_assert_eq!(
            g.bytes(),
            refire.bytes(),
            "refire_guard bytes must match live refire backing"
        );
    }
    Ok(Some((refire, refire_guard)))
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
    // a link's role is under an at-most restriction's when the role or one above it is
    // such a role, which a table of them says without a scan of them for each link
    let mut counted: Vec<bool> = Vec::new();
    for &s in &most {
        if counted.len() <= s as usize {
            counted.resize(s as usize + 1, false);
        }
        counted[s as usize] = true;
    }
    let counts = |s: Role| counted.get(s as usize).copied().unwrap_or(false);
    let back: Vec<Role> = links
        .iter()
        .filter(|&&l| {
            counts(l)
                || roles
                    .sup
                    .get(l as usize)
                    .is_some_and(|v| v.iter().any(|&s| counts(s)))
        })
        .filter_map(|&l| roles.inv(l))
        .collect();
    links.extend(back);
    links.sort_unstable();
    links.dedup();
    // tested for each transition of each automaton below, so by a table: a scan of the
    // links for each transition of a long chain's automaton is their product
    let over_links = roles_over(roles, &links);
    let linked = |s: Role| over_links.get(s as usize).copied().unwrap_or(false);
    // for each automaton, whether a link runs along one of its transitions, found once:
    // the negative existentials below ask it of the same automaton again and again
    let runs: FxHashMap<Role, bool> = h
        .autos
        .iter()
        .map(|(&r, a)| (r, a.trans.iter().any(|t| t.1.is_some_and(linked))))
        .collect();
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
    // A run whose Horn rules fire and that has no empty role composes only the chains
    // that [`select_chains`] keeps (roadmap item 693): a negative existential whose
    // role's inverse automaton no link runs against stays on composed links when its
    // chains are left-linear, and every other one over a role that chains reach becomes
    // a universal on the inverse, unfolded by the automaton.
    let fires = !roles.clash.is_empty()
        || !roles.reflexive.is_empty()
        || it
            .nodes
            .iter()
            .any(|n| matches!(n, Node::Forall(..) | Node::AtMost1(..) | Node::SelfLoop(_)))
        || b.exist.iter().any(|&(f, (r, _))| {
            f != TOP
                && roles.inv(r).is_some_and(|i| match runs.get(&i) {
                    Some(&run) => run,
                    None => linked(i),
                })
        });
    let unlinked = |i: Role| runs.get(&i).is_some_and(|&run| !run);
    // a negative existential over a role that chains reach and whose inverse the run
    // does not have cannot be unfolded, and keeps every chain composed
    let results: Vec<Role> = ax.chains.iter().map(|(_, v)| *v).collect();
    let over_results = roles_over(roles, &results);
    let reached = |r: Role| over_results.get(r as usize).copied().unwrap_or(false);
    let invertible = b
        .exist
        .iter()
        .all(|&(_, (r, _))| roles.inv(r).is_some() || !reached(r));
    let selection = (fires && !roles.any_empty && invertible).then(|| {
        let mut seeds: Vec<Role> = b
            .exist
            .iter()
            .filter(|&&(f, (r, _))| f != TOP && roles.inv(r).is_some_and(unlinked))
            .map(|&(_, (r, _))| r)
            .collect();
        seeds.sort_unstable();
        seeds.dedup();
        select_chains(roles, &ax.chains, &seeds)
    });
    let mut unfold: Vec<(Ex, Role, Ex)> = Vec::new();
    let mut alls: Vec<(Ex, Ex)> = Vec::new();
    for &(f, (r, e)) in &b.exist {
        if f == TOP {
            continue;
        }
        let Some(i) = roles.inv(r) else { continue };
        match runs.get(&i) {
            Some(&run)
                if run
                    || selection
                        .as_ref()
                        .is_some_and(|s| s.composed.binary_search(&r).is_err()) =>
            {
                unfold.push((f, i, e))
            }
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
    // The negative existentials that the universals above give a context are positive now,
    // so their roles have links too, and a range of s holds of the sources of those that
    // run against s as of the others. The range universals that the Horn normalization
    // leaves out rely on it (roadmap item 693).
    let later: Vec<Role> = {
        let roles = &ix.roles;
        let mut v: Vec<Role> = b
            .succ
            .iter()
            .filter_map(|&(e, _)| match it.nodes[e as usize] {
                Node::Exist(r, _) => Some(r),
                _ => None,
            })
            .filter(|r| links.binary_search(r).is_err())
            .collect();
        v.sort_unstable();
        v.dedup();
        let back: Vec<Role> = v
            .iter()
            .filter(|&&l| {
                counts(l)
                    || roles
                        .sup
                        .get(l as usize)
                        .is_some_and(|s| s.iter().any(|&s| counts(s)))
            })
            .filter_map(|&l| roles.inv(l))
            .collect();
        v.extend(back);
        v.sort_unstable();
        v.dedup();
        v
    };
    if !later.is_empty() {
        let over_later = roles_over(&ix.roles, &later);
        let mut news: Vec<(Ex, Ex)> = Vec::new();
        for &(s, c) in &ax.ranges {
            if let Some(i) = ix.roles.inv(s)
                && !linked(i)
                && over_later.get(i as usize).copied().unwrap_or(false)
            {
                news.push((it.node(Node::Exist(i, TOP)), c));
            }
        }
        for &(e, c) in &news {
            b.negative(e, it);
            told.push((e, c));
        }
    }
    // the universals on roles that chains reach, unfolded by the automata, each state a
    // fresh class; the universals the automata make are not unfolded again, in this
    // run or in a later one over the same interner
    let mut flags = vec![0u8; it.nodes.len()];
    for (e, n) in it.nodes.iter().enumerate() {
        if let Node::Forall(r, f) = *n {
            if ix.is_universal(r) {
                // a fact of a universal role, which `Index::build` lists
                flags[e] |= super::horn::HornIx::GLOBAL;
            } else if h.autos.contains_key(&r) && !it.made_by_automaton(e as Ex) {
                flags[e] |= super::horn::HornIx::UNFOLDED;
                unfold.push((e as Ex, r, f));
            }
        }
    }
    // each state of an automaton without ε-transitions is a class: the holder for the
    // start when no transition enters it, the filler for a final state that none leaves,
    // and a fresh class otherwise
    let mut autos: FxHashMap<Role, super::horn::EpsFree> = FxHashMap::default();
    // the universals the transitions make, which the rules apply even when one is a
    // universal that is unfolded too
    let mut made: Vec<Ex> = Vec::new();
    // The roles that a link or a loop of the run can have, with their inverses. The links
    // are those of the existential restrictions with a successor, among them the negative
    // ones that became fillers of universals above, and the results of the chains the run
    // composes. A loop has a reflexive role, the role of a self-restriction, a link's role
    // when an at-most restriction identifies a predecessor with a successor, or a chain's
    // result. Without a selection the run composes every chain and prunes nothing.
    let live_roles: Option<Vec<Role>> = selection.as_ref().map(|sel| {
        let mut v: Vec<Role> = b
            .succ
            .iter()
            .filter_map(|&(e, _)| match it.nodes[e as usize] {
                Node::Exist(r, _) => Some(r),
                _ => None,
            })
            .collect();
        v.extend(sel.comp.values().flatten().copied());
        v.extend(ix.roles.reflexive.iter().copied());
        v.extend(it.nodes.iter().filter_map(|n| match *n {
            Node::SelfLoop(r) => Some(r),
            _ => None,
        }));
        let invs: Vec<Role> = v.iter().filter_map(|&r| ix.roles.inv(r)).collect();
        v.extend(invs);
        v.sort_unstable();
        v.dedup();
        v
    });
    // The roles with a live role under them, as a table: a transition's role is tested
    // by lookups, where a scan of the live roles for each transition of a long chain's
    // automaton is their product.
    let over_live: Option<Vec<bool>> = live_roles.as_ref().map(|v| roles_over(&ix.roles, v));
    // the steps of one lookup in the role hierarchy, for the bound of a reduction
    let longest = ix.roles.sup.iter().map(Vec::len).max().unwrap_or(0);
    let sub_cost = super::horn::log_steps(longest);
    let mut dead: Vec<Role> = Vec::new();
    for (holder, r, f) in unfold {
        let a = autos.entry(r).or_insert_with(|| {
            // R3 reads a universal on `l` at the links under `l`, and R4 at the links
            // and loops under its inverse. A universal role relates every pair.
            let live = over_live.as_ref().map(|over| {
                let ix = &*ix;
                move |l: Role| {
                    let under = |s: Role| over.get(s as usize).copied().unwrap_or(false);
                    ix.is_universal(l) || under(l) || ix.roles.inv(l).is_some_and(under)
                }
            });
            reduced(
                &h.autos[&r],
                &ix.roles,
                sub_cost,
                live.as_ref().map(|f| f as &dyn Fn(Role) -> bool),
                &mut dead,
            )
        });
        // a universal's holder is the universal, and a negative existential
        // restriction's is its filler, which restrictions over other roles share, so
        // the restriction names the scope
        let key = match it.nodes[holder as usize] {
            Node::Forall(t, c) if t == r && c == f => holder,
            _ => f,
        };
        it.begin_scope(Scope::Unfold(key));
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
        it.end_scope();
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
            let x = it.made_universal(l, states[q as usize]);
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
    if let Some(sel) = selection {
        ix.roles.comp = sel.comp;
        ix.roles.left = sel.left;
        ix.roles.right = sel.right;
        // the selection keeps some of the chains, so its table is no larger than the
        // one the role table's build charged
        let _ = ix.roles.finish_chains(|_| Ok::<(), ()>(()));
    }
    dead.sort_unstable();
    dead.dedup();
    dead.shrink_to_fit();
    super::horn::HornIx {
        flags,
        base: it.nodes.len() as Ex,
        dominate: false,
        dead,
    }
}

/// For each role, whether a role of `v` is under it, the roles of `v` included. The table
/// reads the superrole lists of the roles of `v`, which the role table's build walked
/// under its poll.
fn roles_over(roles: &Roles, v: &[Role]) -> Vec<bool> {
    let mut over = vec![false; roles.sup.len()];
    let mut mark = |s: Role| {
        if over.len() <= s as usize {
            over.resize(s as usize + 1, false);
        }
        over[s as usize] = true;
    };
    for &x in v {
        mark(x);
        for &s in roles.sup.get(x as usize).map_or(&[][..], |l| l.as_slice()) {
            mark(s);
        }
    }
    over
}

/// The automaton of a role without ε-transitions, reduced as
/// [`super::horn::reduction`] says.
fn reduced(
    a: &super::horn::Nfa,
    roles: &Roles,
    sub_cost: usize,
    live: Option<&dyn Fn(Role) -> bool>,
    dead: &mut Vec<Role>,
) -> super::horn::EpsFree {
    let mut a = a.eps_free();
    let level = super::horn::reduction();
    if level > 2
        && let Some(live) = live
    {
        a.prune(live, dead);
    }
    if level > 0 {
        a.reduce(level > 1, &|r, s| roles.sub(r, s), sub_cost);
    }
    a
}

/// The chains a run with the Horn rules composes, and the roles whose negative
/// existentials stay on composed links.
struct Selection {
    /// the seeds kept on composed links, sorted
    composed: Vec<Role>,
    comp: FxHashMap<(Role, Role), Vec<Role>>,
    left: Vec<bool>,
    right: Vec<bool>,
}

/// Chooses the chains to compose for the negative existentials over `seeds`, the roles
/// whose inverse automaton no link runs against (roadmap item 693).
///
/// The chains are the told ones and their mirror images. The closure `N(t)` of a seed
/// starts with `t`, and each chain whose result is under a role of the closure adds its
/// left operand. It adds its right operand too unless the chain is left-linear,
/// `s1 ∘ s2 ⊑ v` with `v ⊑ s1`, and `s2` is letter-closed: every chain whose result is
/// under `s2` has all its operands under `s2`. Then each `s2`-path is a path of links by
/// roles under `s2`, and the links of `v` come from the complete links of `s1` and the
/// direct links under `s2`, one at a time, so that no chain into `s2` is composed. A seed
/// stays on composed links when every chain of its closure has two operands, and the
/// others are unfolded as every negative existential over a role that chains reach is.
/// The run composes the chains of the closures and no others.
fn select_chains(roles: &Roles, told: &[(Vec<Role>, Role)], seeds: &[Role]) -> Selection {
    let n = roles.sup.len();
    let mut chains: Vec<(Vec<Role>, Role)> = Vec::with_capacity(2 * told.len());
    for (ops, t) in told {
        chains.push((ops.clone(), *t));
        let mirror: Option<Vec<Role>> = ops.iter().rev().map(|&o| roles.inv(o)).collect();
        if let (Some(m), Some(i)) = (mirror, roles.inv(*t)) {
            chains.push((m, i));
        }
    }
    chains.sort();
    chains.dedup();
    let letter_closed = |s2: Role| {
        chains
            .iter()
            .filter(|(_, z)| roles.sub(*z, s2))
            .all(|(ops, _)| ops.iter().all(|&o| roles.sub(o, s2)))
    };
    // the chains of a seed's closure
    let closure = |t: Role| -> Option<Vec<usize>> {
        let mut need = vec![t];
        let mut picked: Vec<usize> = Vec::new();
        loop {
            let mut grew = false;
            for (k, (ops, v)) in chains.iter().enumerate() {
                if picked.contains(&k) || !need.iter().any(|&u| roles.sub(*v, u)) {
                    continue;
                }
                let [s1, s2] = ops[..] else { return None };
                picked.push(k);
                grew = true;
                let direct = roles.sub(*v, s1) && letter_closed(s2);
                for o in [Some(s1), (!direct).then_some(s2)].into_iter().flatten() {
                    if !need.contains(&o) {
                        need.push(o);
                    }
                }
            }
            if !grew {
                break;
            }
        }
        Some(picked)
    };
    let mut composed: Vec<Role> = Vec::new();
    let mut picked: Vec<usize> = Vec::new();
    for &t in seeds {
        if let Some(ks) = closure(t) {
            composed.push(t);
            picked.extend(ks);
        }
    }
    picked.sort_unstable();
    picked.dedup();
    let mut sub: Vec<Vec<Role>> = vec![Vec::new(); n];
    for (r, ss) in roles.sup.iter().enumerate() {
        for &s in ss {
            sub[s as usize].push(r as Role);
        }
    }
    let mut comp: FxHashMap<(Role, Role), Vec<Role>> = FxHashMap::default();
    let mut left = vec![false; n];
    let mut right = vec![false; n];
    for &k in &picked {
        let (ops, v) = &chains[k];
        for &r1 in &sub[ops[0] as usize] {
            left[r1 as usize] = true;
            for &r2 in &sub[ops[1] as usize] {
                right[r2 as usize] = true;
                comp.entry((r1, r2)).or_default().push(*v);
            }
        }
    }
    for v in comp.values_mut() {
        // the role table's build polled the stop over the same superrole lists
        let _ = minimal_roles(
            v,
            |r| roles.sup[r as usize].as_slice(),
            |_| Ok::<(), ()>(()),
        );
    }
    composed.sort_unstable();
    Selection {
        composed,
        comp,
        left,
        right,
    }
}

/// Keeps the least roles of `v`, the roles that a chain composes a pair of links into,
/// where `sup(r)` lists r's superroles with r itself, sorted. A role above another role of
/// `v` is dropped, and of equivalent roles the one with the smallest id stays. Every rule
/// reads a link's role through its superroles, and the composition table lists every pair
/// of roles under a chain's operands, so a link by the least role counts for the others.
/// Without the reduction a symmetric transitive role, which is its own inverse, composed
/// each link once by the role and once by its inverse, since the Horn rules add the mirror
/// image of its transitivity. `tick` takes the work of each role as the reduction goes,
/// the length of its superrole list, so that the caller polls its stop.
fn minimal_roles<'a, E>(
    v: &mut Vec<Role>,
    sup: impl Fn(Role) -> &'a [Role],
    mut tick: impl FnMut(usize) -> Result<(), E>,
) -> Result<(), E> {
    v.sort_unstable();
    v.dedup();
    if v.len() < 2 {
        return Ok(());
    }
    let mut dropped = vec![false; v.len()];
    for &u in v.iter() {
        let above = sup(u);
        tick(1 + above.len())?;
        for &s in above {
            let Ok(k) = v.binary_search(&s) else { continue };
            // u is under s: s goes, unless it is equivalent to u and comes first
            if s != u && !(s < u && sup(s).binary_search(&u).is_ok()) {
                dropped[k] = true;
            }
        }
    }
    let mut k = 0;
    v.retain(|_| {
        k += 1;
        !dropped[k - 1]
    });
    Ok(())
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
    /// the told subsumers of positive data restrictions and empty roles, and the ranges
    /// of the individuals that an assertion gives a role's successor
    told: Vec<(Ex, Ex)>,
    /// the existential restrictions `∃r.{b}` that are told of an individual, over a
    /// role with a range
    asserted: FxHashSet<Ex>,
    /// the checker's atoms in negative positions, in the order first met
    dt_body: Vec<Ex>,
    /// those with an occurrence other than as the left side of a told subsumption
    dt_other: FxHashSet<Ex>,
}

impl Builder {
    /// Registers a negative occurrence: the rules that build the expression from its
    /// parts.
    fn negative(&mut self, e: Ex, it: &Interner) {
        self.negative_as(e, it, false)
    }

    /// [`Builder::negative`], where `told` says that `e` is the left side of a told
    /// subsumption, the one occurrence of a checker atom whose truth the rule can leave
    /// to the subsumer.
    fn negative_as(&mut self, root: Ex, it: &Interner, told: bool) {
        let mut stack = vec![root];
        while let Some(e) = stack.pop() {
            // marked before the dedup, since every occurrence counts
            if let Node::DtAtom(_) = it.nodes[e as usize]
                && !(told && e == root)
            {
                self.dt_other.insert(e);
            }
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
                Node::DtAtom(_) => self.dt_body.push(e),
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
                    // an individual has the successor, which is the filler's individual
                    // and so in the range
                    Some(range) if self.asserted.contains(&e) => {
                        self.positive(range, it, ix);
                        if f != range {
                            self.told.push((f, range));
                        }
                        f
                    }
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
                        let u = it.prefix_role(left, r);
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
    if let Some(top) = it.role_id(&RoleName::Object(ObjectProperty::top())) {
        for (r, name) in it.roles.iter().enumerate() {
            if matches!(name, RoleName::Object(_)) && r as Role != top {
                direct[r].push(top);
            }
        }
    }
    if let Some(top) = it.role_id(&RoleName::Data(DataProperty::top())) {
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
        minimal_roles(v, |r| sup[r as usize].as_slice(), &tick)?;
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
    let mut any_functional = false;
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
                any_functional = true;
            }
        }
        if let Some(v) = acc {
            data_range[r] = Some(it.val(v));
            it.ranges.check_halt()?;
        }
    }
    // a role under the bottom property relates nothing
    let mut empty = vec![false; n];
    for bottom in [
        RoleName::Object(ObjectProperty::bottom()),
        RoleName::Data(DataProperty::bottom()),
    ] {
        if let Some(b) = it.role_id(&bottom) {
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
        None if ax.dl.is_some() => (super::horn::inverses(it), Vec::new()),
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
        any_functional,
        any_empty: false,
        empty,
        inv,
        clash,
        linear: Vec::new(),
        comp_fwd: FxHashMap::default(),
    };
    if !roles.clash.is_empty() {
        for r in 0..n as Role {
            if roles.clashes(r, r) {
                roles.empty[r as usize] = true;
            }
        }
    }
    roles.any_empty = roles.empty.contains(&true);
    // the products of the chains read the empty roles
    roles.finish_chains(grow)?;
    Ok(roles)
}

// Count real opaque ownership reads. K needs no such accounting and never calls record.
#[cfg(test)]
thread_local! {
    static A1_ACCOUNTING_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    // Last `(seen_init_cap, seen_final_cap, changed_init_cap, changed_final_cap)` observed
    // by `Index::changed_with_dt`'s negative-role comparison loop.
    static NEG_ROLE_SET_STATS: std::cell::Cell<Option<(usize, usize, usize, usize)>> =
        const { std::cell::Cell::new(None) };
    // `(compactions, entries_inspected)` recorded inside `dt_fires`'s compaction branch.
    static DT_FIRES_COMPACT_STATS: std::cell::Cell<(usize, usize)> =
        const { std::cell::Cell::new((0, 0)) };
}
#[cfg(test)]
fn dt_fires_record_compaction(entries: usize) {
    DT_FIRES_COMPACT_STATS.with(|s| {
        let (count, inspected) = s.get();
        s.set((count + 1, inspected + entries));
    });
}
#[cfg(test)]
fn reset_dt_fires_compaction_stats() {
    DT_FIRES_COMPACT_STATS.with(|s| s.set((0, 0)));
}
#[cfg(test)]
fn dt_fires_compaction_stats() -> (usize, usize) {
    DT_FIRES_COMPACT_STATS.with(std::cell::Cell::get)
}
#[cfg(test)]
fn neg_role_set_record(
    seen_init: usize,
    seen_final: usize,
    changed_init: usize,
    changed_final: usize,
) {
    NEG_ROLE_SET_STATS.with(|s| s.set(Some((seen_init, seen_final, changed_init, changed_final))));
}
#[cfg(test)]
pub(super) fn reset_neg_role_set_stats() {
    NEG_ROLE_SET_STATS.with(|s| s.set(None));
}
#[cfg(test)]
pub(super) fn neg_role_set_stats() -> Option<(usize, usize, usize, usize)> {
    NEG_ROLE_SET_STATS.with(std::cell::Cell::get)
}
#[cfg(test)]
fn a1_record_accounting_visit() {
    A1_ACCOUNTING_VISITS.with(|n| n.set(n.get() + 1));
}
#[cfg(test)]
fn a1_reset_accounting_visits() {
    A1_ACCOUNTING_VISITS.with(|n| n.set(0));
}
#[cfg(test)]
fn a1_accounting_visits() -> usize {
    A1_ACCOUNTING_VISITS.with(std::cell::Cell::get)
}
#[cfg(test)]
#[path = "a1_regression_probes.rs"]
mod a1_regression_probes;
#[cfg(test)]
#[path = "a1_repair_accounting_tests.rs"]
mod a1_repair_accounting_tests;
#[cfg(test)]
mod minimal_roles_tests {
    use super::{Role, minimal_roles};

    /// `sup` from direct superroles, closed and sorted, each role with itself.
    fn closed(n: usize, direct: &[(Role, Role)]) -> Vec<Vec<Role>> {
        (0..n as Role)
            .map(|r| {
                let mut seen = vec![r];
                let mut i = 0;
                while i < seen.len() {
                    let x = seen[i];
                    for &(a, b) in direct {
                        if a == x && !seen.contains(&b) {
                            seen.push(b);
                        }
                    }
                    i += 1;
                }
                seen.sort_unstable();
                seen
            })
            .collect()
    }

    #[test]
    fn keeps_the_least_roles_and_the_first_of_equivalent_ones() {
        // 0 ⊑ 1, 2 ≡ 3 ≡ 5, 4 unrelated, 3 ⊑ 6
        let sup = closed(7, &[(0, 1), (2, 3), (3, 5), (5, 2), (3, 6)]);
        let none = |_| Ok::<(), ()>(());
        let mut v = vec![6, 5, 4, 3, 2, 1, 0, 3];
        minimal_roles(&mut v, |r| sup[r as usize].as_slice(), none).unwrap();
        assert_eq!(v, vec![0, 2, 4]);
        // the survivor of equivalent roles does not depend on the order given
        let mut v = vec![5, 3];
        minimal_roles(&mut v, |r| sup[r as usize].as_slice(), none).unwrap();
        assert_eq!(v, vec![3]);
    }

    #[test]
    fn many_unrelated_roles_take_linear_work() {
        let n = 200_000;
        let sup: Vec<Vec<Role>> = (0..n as Role).map(|r| vec![r]).collect();
        let mut v: Vec<Role> = (0..n as Role).rev().collect();
        let mut work = 0;
        minimal_roles(
            &mut v,
            |r| sup[r as usize].as_slice(),
            |w| {
                work += w;
                Ok::<(), ()>(())
            },
        )
        .unwrap();
        assert_eq!(v.len(), n);
        assert_eq!(work, 2 * n);
    }

    #[test]
    fn a_stop_ends_the_reduction_as_it_goes() {
        let n = 10_000;
        let sup: Vec<Vec<Role>> = (0..n as Role).map(|r| vec![r]).collect();
        let mut v: Vec<Role> = (0..n as Role).collect();
        let mut calls = 0;
        let r = minimal_roles(
            &mut v,
            |r| sup[r as usize].as_slice(),
            |_| {
                calls += 1;
                if calls == 3 { Err(()) } else { Ok(()) }
            },
        );
        assert_eq!(r, Err(()));
        assert_eq!(calls, 3);
    }

    #[test]
    fn dt_fires_reserves_sort_vector_through_build_budget() {
        use super::super::Limits;
        use super::super::dtcheck::{
            atoms::{DtKey, DtRun, Stop},
            comp::Comps,
            range::{Defs, Ranges as DtRanges},
        };
        use super::super::numeric::Budget;
        use super::{Builder, Ex, Halt, Interner, Node, RoleName, dt_fires};
        use crate::model::{AxiomKind, Class, ClassExpression, DataProperty, DataRange, Datatype};
        use crate::vocab;

        let n_atoms = 4000usize;
        let dp = DataProperty::new("urn:dt:p");
        let target_cls = Class::new("urn:dt:Target");
        let ax = AxiomKind::SubClassOf(
            ClassExpression::Class(target_cls.clone()),
            ClassExpression::DataAllValuesFrom(
                vec![dp.clone()],
                DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
            ),
        );
        let comps = Comps::of(&[&ax], &Defs::default(), &|| false).expect("checker comp");
        assert!(comps.checker(&dp));
        let mut it = Interner::new();
        let role = it.role(RoleName::Data(dp));
        let target = it.class(&target_cls);
        let mut b = Builder::default();
        let mut told_base: Vec<(Ex, Ex)> = Vec::with_capacity(n_atoms);
        for i in 0..n_atoms {
            let id = it.dt.id(DtKey {
                role,
                at_least: true,
                n: (i + 1) as u64,
                range: DtRanges::FULL,
            });
            let e = it.node(Node::DtAtom(id));
            b.dt_body.push(e);
            told_base.push((e, target));
        }
        let run = DtRun {
            comps,
            defs: Defs::default(),
            stop: Stop::default(),
            budget: None,
            stopped: None,
        };
        let expected_eligible_bytes = n_atoms * std::mem::size_of::<(Ex, u32)>();

        // 1. Uncapped run must charge `eligible` + live `refire` against `it.ranges.budget`,
        //    keep `refire` charged via its returned guard, and release scratch once dropped.
        let uncap = Budget::new(&Limits::default(), 0, None);
        it.ranges.begin(uncap.clone());
        let mut told = told_base.clone();
        let (refire, refire_guard) = dt_fires(&mut it, &run, &b, &mut told).expect("uncapped");
        assert_eq!(refire.table.len(), 1);
        assert_eq!(refire.get(&target).map(Vec::len), Some(1));
        assert!(
            uncap.peak() >= expected_eligible_bytes,
            "expected dt_fires peak {} >= eligible bytes {expected_eligible_bytes}",
            uncap.peak()
        );
        let refire1_bytes = refire.bytes();
        assert_eq!(
            refire_guard.as_ref().expect("live refire guard").bytes(),
            refire1_bytes,
            "carried refire guard must match exact final live backing"
        );
        assert_eq!(
            uncap.retained_storage().1,
            refire1_bytes,
            "live refire guard must keep exact refire bytes charged until consumed or dropped"
        );
        drop((refire, refire_guard));
        it.ranges.end();
        assert_eq!(uncap.retained_storage().1, 0, "scratch must be released");

        // Tight cap below `eligible` must refuse before allocating `eligible`.
        let tight = Budget::new(
            &Limits {
                max_memory: Some(expected_eligible_bytes - 1),
                ..Limits::default()
            },
            0,
            None,
        );
        it.ranges.begin(tight);
        let mut told = told_base;
        let err = dt_fires(&mut it, &run, &b, &mut told);
        it.ranges.end();
        assert!(
            matches!(err, Err(Halt::Memory(_))),
            "expected Halt::Memory under tight cap, got {:?}",
            err.err()
        );

        // 2. Overlapping `eligible` + multi-target `refire` growth must both be charged
        //    concurrently, and alternating duplicate components must stay bounded.
        let dp0 = DataProperty::new("urn:dt:p0");
        let dp1 = DataProperty::new("urn:dt:p1");
        let ax0 = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("urn:dt:A0")),
            ClassExpression::DataAllValuesFrom(
                vec![dp0.clone()],
                DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
            ),
        );
        let ax1 = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("urn:dt:A1")),
            ClassExpression::DataAllValuesFrom(
                vec![dp1.clone()],
                DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
            ),
        );
        let comps2 = Comps::of(&[&ax0, &ax1], &Defs::default(), &|| false).expect("2 comps");
        let c0 = comps2.component(&dp0).expect("comp 0");
        let c1 = comps2.component(&dp1).expect("comp 1");
        assert_ne!(c0, c1);
        let mut it2 = Interner::new();
        let r0 = it2.role(RoleName::Data(dp0));
        let r1 = it2.role(RoleName::Data(dp1));
        let alt = it2.class(&Class::new("urn:dt:Alt"));
        let m = 1000usize;
        let mut b2 = Builder::default();
        let mut told2_base: Vec<(Ex, Ex)> = Vec::with_capacity(m * 2);
        for i in 0..m {
            let role = if i % 2 == 0 { r0 } else { r1 };
            let id = it2.dt.id(DtKey {
                role,
                at_least: true,
                n: (i + 1) as u64,
                range: DtRanges::FULL,
            });
            let e = it2.node(Node::DtAtom(id));
            let t = it2.class(&Class::new(format!("urn:dt:T{i}")));
            b2.dt_body.push(e);
            told2_base.push((e, t));
            told2_base.push((e, alt));
        }
        let run2 = DtRun {
            comps: comps2,
            defs: Defs::default(),
            stop: Stop::default(),
            budget: None,
            stopped: None,
        };
        let eligible2_bytes = m * std::mem::size_of::<(Ex, u32)>();

        let uncap2 = Budget::new(&Limits::default(), 0, None);
        it2.ranges.begin(uncap2.clone());
        let mut told2 = told2_base.clone();
        let (refire2, guard2) = dt_fires(&mut it2, &run2, &b2, &mut told2).expect("uncapped 2");
        assert_eq!(refire2.table.len(), m + 1);
        let alt_comps = refire2.get(&alt).expect("alt refire");
        let mut expected_alt = vec![c0, c1];
        expected_alt.sort_unstable();
        assert_eq!(alt_comps.as_slice(), expected_alt.as_slice());
        assert!(
            alt_comps.capacity() <= 4,
            "alternating duplicates must not bloat component vec capacity, got {}",
            alt_comps.capacity()
        );
        let refire2_bytes = refire2.bytes();
        assert_eq!(
            guard2.as_ref().expect("live refire2 guard").bytes(),
            refire2_bytes,
            "carried refire2 guard must release superseded map rehashes and match final backing"
        );
        assert_eq!(uncap2.retained_storage().1, refire2_bytes);
        assert!(
            uncap2.peak() >= eligible2_bytes + refire2_bytes,
            "peak {} must cover overlapping eligible ({eligible2_bytes}) + refire ({refire2_bytes})",
            uncap2.peak()
        );
        drop((refire2, guard2));
        it2.ranges.end();
        assert_eq!(uncap2.retained_storage().1, 0);

        // A cap at the true live peak must succeed (cumulative historical rehash charges would
        // exceed it and spuriously fail).
        let exact2 = Budget::new(
            &Limits {
                max_memory: Some(uncap2.peak()),
                ..Limits::default()
            },
            0,
            None,
        );
        it2.ranges.begin(exact2);
        let mut told2 = told2_base.clone();
        let (refire2_exact, guard2_exact) =
            dt_fires(&mut it2, &run2, &b2, &mut told2).expect("live-peak cap must succeed");
        assert_eq!(guard2_exact.as_ref().expect("guard").bytes(), refire2_bytes);
        drop((refire2_exact, guard2_exact));
        it2.ranges.end();

        // Cap strictly above `eligible2_bytes` (which would pass an `items`-only guard) must
        // refuse while `refire` grows alongside live `eligible`.
        let tight2 = Budget::new(
            &Limits {
                max_memory: Some(eligible2_bytes + 1),
                ..Limits::default()
            },
            0,
            None,
        );
        it2.ranges.begin(tight2);
        let mut told2 = told2_base;
        let err2 = dt_fires(&mut it2, &run2, &b2, &mut told2);
        it2.ranges.end();
        assert!(
            matches!(err2, Err(Halt::Memory(_))),
            "expected Halt::Memory when refire overlaps live eligible, got {:?}",
            err2.err()
        );
    }

    #[test]
    fn dt_fires_near_full_compaction_avoids_quadratic_resorting() {
        use super::super::dtcheck::{
            atoms::{DtKey, DtRun, Stop},
            comp::Comps,
            range::{Defs, Ranges as DtRanges},
        };
        use super::{
            Builder, Ex, Interner, Node, RoleName, dt_fires, dt_fires_compaction_stats,
            reset_dt_fires_compaction_stats,
        };
        use crate::model::{AxiomKind, Class, ClassExpression, DataProperty, DataRange, Datatype};
        use crate::vocab;

        let k = 128usize;
        let tail = 1000usize;
        let props: Vec<DataProperty> = (0..k)
            .map(|i| DataProperty::new(format!("urn:dt:nf_p{i}")))
            .collect();
        let axioms: Vec<AxiomKind> = props
            .iter()
            .enumerate()
            .map(|(i, dp)| {
                AxiomKind::SubClassOf(
                    ClassExpression::Class(Class::new(format!("urn:dt:NF_A{i}"))),
                    ClassExpression::DataAllValuesFrom(
                        vec![dp.clone()],
                        DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
                    ),
                )
            })
            .collect();
        let refs: Vec<&AxiomKind> = axioms.iter().collect();
        let comps = Comps::of(&refs, &Defs::default(), &|| false).expect("k comps");

        // Cover both `K - 1` (near-full at capacity K) and `K` (full at capacity K) distinct
        // components followed by a long alternating duplicate tail over the first two components.
        for distinct in [k - 1, k] {
            let mut it = Interner::new();
            let mut role_comps: Vec<(Role, u32)> = props
                .iter()
                .map(|dp| {
                    let c = comps.component(dp).expect("checker comp");
                    (it.role(RoleName::Data(dp.clone())), c)
                })
                .collect();
            role_comps.sort_unstable_by_key(|&(_, c)| c);
            let target = it.class(&Class::new("urn:dt:NFTarget"));
            let mut b = Builder::default();
            let mut told: Vec<(Ex, Ex)> = Vec::with_capacity(distinct + tail);
            for (idx, &(role, _)) in role_comps.iter().take(distinct).enumerate() {
                let id = it.dt.id(DtKey {
                    role,
                    at_least: true,
                    n: (idx + 1) as u64,
                    range: DtRanges::FULL,
                });
                let e = it.node(Node::DtAtom(id));
                b.dt_body.push(e);
                told.push((e, target));
            }
            let r0 = role_comps[0].0;
            let r1 = role_comps[1].0;
            for j in 0..tail {
                let role = if j % 2 == 0 { r0 } else { r1 };
                let id = it.dt.id(DtKey {
                    role,
                    at_least: true,
                    n: (distinct + j + 1) as u64,
                    range: DtRanges::FULL,
                });
                let e = it.node(Node::DtAtom(id));
                b.dt_body.push(e);
                told.push((e, target));
            }
            let run = DtRun {
                comps: comps.clone(),
                defs: Defs::default(),
                stop: Stop::default(),
                budget: None,
                stopped: None,
            };
            reset_dt_fires_compaction_stats();
            let (refire, _guard) = dt_fires(&mut it, &run, &b, &mut told).expect("dt_fires");
            let (compactions, inspected) = dt_fires_compaction_stats();
            let cs = refire.get(&target).expect("target refire");
            let expected: Vec<u32> = role_comps.iter().take(distinct).map(|&(_, c)| c).collect();
            assert_eq!(cs.as_slice(), expected.as_slice());
            assert!(
                cs.capacity() <= 2 * k,
                "distinct={distinct}: capacity {} must stay bounded within 2*K ({})",
                cs.capacity(),
                2 * k
            );
            assert!(
                compactions <= 20,
                "distinct={distinct}: expected O(T/K) compactions (<= 20), got {compactions}"
            );
            assert!(
                inspected <= 4 * (distinct + tail),
                "distinct={distinct}: expected linear inspected entries (<= {}), got {inspected}",
                4 * (distinct + tail)
            );
        }
    }

    #[test]
    fn dt_fires_releases_superseded_vector_growth_charges() {
        use super::super::Limits;
        use super::super::dtcheck::{
            atoms::{DtKey, DtRun, Stop},
            comp::Comps,
            range::{Defs, Ranges as DtRanges},
        };
        use super::super::numeric::Budget;
        use super::{Builder, Ex, Halt, Interner, Node, RoleName, dt_fires};
        use crate::model::{AxiomKind, Class, ClassExpression, DataProperty, DataRange, Datatype};
        use crate::vocab;

        // 1,024 distinct components targeting one class grows the component vector from
        // 4 -> 8 -> ... -> 1,024. The live old+new overlap peak is 512 + 1,024 = 1,536 entries,
        // while cumulative historical growth would charge 2,044 entries.
        let k = 1024usize;
        let props: Vec<DataProperty> = (0..k)
            .map(|i| DataProperty::new(format!("urn:dt:vg_p{i}")))
            .collect();
        let axioms: Vec<AxiomKind> = props
            .iter()
            .enumerate()
            .map(|(i, dp)| {
                AxiomKind::SubClassOf(
                    ClassExpression::Class(Class::new(format!("urn:dt:VG_A{i}"))),
                    ClassExpression::DataAllValuesFrom(
                        vec![dp.clone()],
                        DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
                    ),
                )
            })
            .collect();
        let refs: Vec<&AxiomKind> = axioms.iter().collect();
        let comps = Comps::of(&refs, &Defs::default(), &|| false).expect("1024 comps");
        let mut it = Interner::new();
        let target = it.class(&Class::new("urn:dt:VGTarget"));
        let mut b = Builder::default();
        let mut told_base: Vec<(Ex, Ex)> = Vec::with_capacity(k);
        for (i, dp) in props.iter().enumerate() {
            let role = it.role(RoleName::Data(dp.clone()));
            let id = it.dt.id(DtKey {
                role,
                at_least: true,
                n: (i + 1) as u64,
                range: DtRanges::FULL,
            });
            let e = it.node(Node::DtAtom(id));
            b.dt_body.push(e);
            told_base.push((e, target));
        }
        let run = DtRun {
            comps,
            defs: Defs::default(),
            stop: Stop::default(),
            budget: None,
            stopped: None,
        };
        let eligible_bytes = k * std::mem::size_of::<(Ex, u32)>();

        let uncap = Budget::new(&Limits::default(), 0, None);
        it.ranges.begin(uncap.clone());
        let mut told = told_base.clone();
        let (refire, guard) = dt_fires(&mut it, &run, &b, &mut told).expect("uncapped");
        let cs = refire.get(&target).expect("target refire");
        assert_eq!(cs.len(), k);
        assert_eq!(cs.capacity(), k);
        let map_bytes = refire.table_bytes();
        let final_refire_bytes = map_bytes + k * std::mem::size_of::<u32>();
        assert_eq!(
            guard.as_ref().expect("guard").bytes(),
            final_refire_bytes,
            "carried refire guard must match live final backing"
        );
        assert_eq!(uncap.retained_storage().1, final_refire_bytes);
        let expected_live_peak =
            eligible_bytes + map_bytes + (k / 2 + k) * std::mem::size_of::<u32>();
        let cumulative_peak = eligible_bytes + map_bytes + (2 * k - 4) * std::mem::size_of::<u32>();
        assert!(expected_live_peak < cumulative_peak);
        assert_eq!(
            uncap.peak(),
            expected_live_peak,
            "peak must cover eligible + map + 512..1024 overlap without accumulating prior backings"
        );
        drop((refire, guard));
        it.ranges.end();
        assert_eq!(uncap.retained_storage().1, 0);

        // Cap at the true live peak succeeds (would fail under cumulative growth accounting).
        let live_cap = Budget::new(
            &Limits {
                max_memory: Some(expected_live_peak),
                ..Limits::default()
            },
            0,
            None,
        );
        it.ranges.begin(live_cap);
        let mut told = told_base.clone();
        let (refire_ok, guard_ok) =
            dt_fires(&mut it, &run, &b, &mut told).expect("live-peak cap must succeed");
        assert_eq!(
            guard_ok.as_ref().expect("guard").bytes(),
            final_refire_bytes
        );
        drop((refire_ok, guard_ok));
        it.ranges.end();

        // Cap one byte below the 512 + 1,024 overlap peak must still refuse.
        let under_cap = Budget::new(
            &Limits {
                max_memory: Some(expected_live_peak - 1),
                ..Limits::default()
            },
            0,
            None,
        );
        it.ranges.begin(under_cap);
        let mut told = told_base;
        let err = dt_fires(&mut it, &run, &b, &mut told);
        it.ranges.end();
        assert!(
            matches!(err, Err(Halt::Memory(_))),
            "expected Halt::Memory one byte below live overlap peak, got {:?}",
            err.err()
        );
    }

    #[test]
    fn dt_fires_single_target_fits_under_prior_peak() {
        dt_fires_at_prior_peak(&[1], &[], 4, 4);
    }

    #[test]
    fn dt_fires_reversed_growth_fits_under_prior_peak() {
        // A and B grew on their duplicate edges, in that order. Growing on the
        // fresh components instead would reverse the order and need 512 slots.
        // Replay uses the prior growth order, which peaks at 448.
        dt_fires_at_prior_peak(&[128, 64], &[(1, 64), (0, 128)], 448, 384);
    }

    #[test]
    fn dt_fires_retained_vectors_fit_under_prior_index_peak() {
        use super::super::Limits;
        use super::super::dtcheck::{
            atoms::{DtKey, DtRun, Stop},
            comp::Comps,
            range::{Defs, Ranges as DtRanges},
        };
        use super::super::numeric::Budget;
        use super::{Axioms, Halt, Index, Interner, Node, RoleName};
        use crate::model::{AxiomKind, Class, ClassExpression, DataProperty, DataRange, Datatype};
        use crate::vocab;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let class = Class::new("urn:dt:RefireIndexTarget");
        let mut props: Vec<_> = (0..8)
            .map(|i| DataProperty::new(format!("urn:dt:RefireIndexP{i}")))
            .collect();
        let axioms: Vec<_> = props
            .iter()
            .map(|dp| {
                AxiomKind::SubClassOf(
                    ClassExpression::Class(class.clone()),
                    ClassExpression::DataAllValuesFrom(
                        vec![dp.clone()],
                        DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
                    ),
                )
            })
            .collect();
        let comps = Comps::of(
            &axioms.iter().collect::<Vec<_>>(),
            &Defs::default(),
            &|| false,
        )
        .expect("checker components");
        props.sort_unstable_by_key(|dp| comps.component(dp).expect("component"));
        let sequence = [0, 1, 2, 3, 0, 1, 0, 1, 4, 0, 5, 6, 7];
        let build = |group_duplicates, limits: &Limits| {
            let mut it = Interner::new();
            let target = it.class(&class);
            let roles: Vec<_> = props
                .iter()
                .map(|dp| it.role(RoleName::Data(dp.clone())))
                .collect();
            let mut edges: Vec<_> = sequence
                .iter()
                .enumerate()
                .map(|(i, &comp)| {
                    let id = it.dt.id(DtKey {
                        role: roles[comp],
                        at_least: true,
                        n: (i + 1) as u64,
                        range: DtRanges::FULL,
                    });
                    (comp, (it.node(Node::DtAtom(id)), target))
                })
                .collect();
            if group_duplicates {
                // Adjacent duplicates are skipped in both growth orders. The eight
                // distinct components then finish at capacity eight in both scans.
                edges.sort_unstable_by_key(|&(comp, _)| comp);
            }
            let ax = Axioms {
                subs: edges.into_iter().map(|(_, edge)| edge).collect(),
                ..Axioms::default()
            };
            it.dt_run = Some(Box::new(DtRun {
                comps: comps.clone(),
                defs: Defs::default(),
                stop: Stop::default(),
                budget: None,
                stopped: None,
            }));
            let budget = Budget::new(limits, 0, None);
            it.ranges.begin(budget.clone());
            let result = Index::build(&ax, &mut it, None, &|| false);
            it.ranges.end();
            assert_eq!(budget.retained_storage().1, 0);
            result.map(|ix| {
                let told = ix.told_of(ix.head(target));
                assert_eq!(told.len(), 1);
                assert_eq!(it.nodes[told[0] as usize], Node::DtFire(target));
                let dt = ix.dt.expect("datatype index");
                assert_eq!(dt.refire_of(target), Some(&[0, 1, 2, 3, 4, 5, 6, 7][..]));
                (budget.peak(), dt.bytes())
            })
        };
        let (cap, retained) = build(true, &Limits::default()).expect("grouped control");
        let capped = |limit| Limits {
            max_memory: Some(limit),
            ..Limits::default()
        };
        assert!(matches!(
            build(false, &capped(cap - 1)),
            Err(Halt::Memory(_))
        ));
        assert_eq!(
            build(false, &capped(cap)).expect("prior index peak must fit"),
            (cap, retained)
        );
        let paid = Arc::new(AtomicUsize::new(0));
        let pool = paid.clone();
        let pooled = Limits {
            charge: Some(Arc::new(move |more| {
                pool.try_update(Ordering::Relaxed, Ordering::Relaxed, |paid| {
                    paid.checked_add(more).filter(|&total| total <= cap)
                })
                .is_ok()
            })),
            ..Limits::default()
        };
        assert_eq!(
            build(false, &pooled).expect("prior index pool peak must fit"),
            (cap, retained)
        );
        assert_eq!(paid.load(Ordering::Relaxed), cap);
    }

    fn dt_fires_at_prior_peak(
        distinct: &[usize],
        appends: &[(usize, usize)],
        peak_slots: usize,
        retained_slots: usize,
    ) {
        use super::super::Limits;
        use super::super::dtcheck::{
            atoms::{DtKey, DtRun, Stop},
            comp::Comps,
            range::{Defs, Ranges as DtRanges, map_buckets_for},
        };
        use super::super::numeric::Budget;
        use super::{Builder, Ex, Halt, Interner, Node, RoleName, dt_fires};
        use crate::model::{AxiomKind, Class, ClassExpression, DataProperty, DataRange, Datatype};
        use crate::vocab;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let n_props = distinct[0] + usize::from(!appends.is_empty());
        let props: Vec<_> = (0..n_props)
            .map(|i| DataProperty::new(format!("urn:dt:PriorCapP{i}")))
            .collect();
        let axioms: Vec<_> = props
            .iter()
            .map(|dp| {
                AxiomKind::SubClassOf(
                    ClassExpression::Class(Class::new("urn:dt:PriorCapBody")),
                    ClassExpression::DataAllValuesFrom(
                        vec![dp.clone()],
                        DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
                    ),
                )
            })
            .collect();
        let refs: Vec<_> = axioms.iter().collect();
        let run = DtRun {
            comps: Comps::of(&refs, &Defs::default(), &|| false).expect("checker components"),
            defs: Defs::default(),
            stop: Stop::default(),
            budget: None,
            stopped: None,
        };
        let mut it = Interner::new();
        let targets: Vec<_> = (0..distinct.len())
            .map(|i| it.class(&Class::new(format!("urn:dt:PriorCapT{i}"))))
            .collect();
        let mut b = Builder::default();
        let mut atoms: Vec<_> = props
            .iter()
            .map(|dp| {
                let role = it.role(RoleName::Data(dp.clone()));
                let id = it.dt.id(DtKey {
                    role,
                    at_least: true,
                    n: 1,
                    range: DtRanges::FULL,
                });
                (
                    run.comps.component(dp).expect("component"),
                    role,
                    it.node(Node::DtAtom(id)),
                )
            })
            .collect();
        atoms.sort_unstable();
        b.dt_body.extend(atoms.iter().map(|&(_, _, atom)| atom));
        let mut told = Vec::new();
        for (&target, &n) in targets.iter().zip(distinct) {
            told.extend(atoms[..n].iter().map(|&(_, _, atom)| (atom, target)));
        }
        if !appends.is_empty() {
            let duplicate = it.dt.id(DtKey {
                role: atoms[0].1,
                at_least: true,
                n: 2,
                range: DtRanges::FULL,
            });
            let duplicate = it.node(Node::DtAtom(duplicate));
            b.dt_body.push(duplicate);
            told.extend(targets.iter().map(|&target| (duplicate, target)));
            b.told
                .extend(appends.iter().map(|&(t, c)| (atoms[c].2, targets[t])));
        }
        let eligible_bytes = b.dt_body.len() * std::mem::size_of::<(Ex, u32)>();
        let map_bytes =
            map_buckets_for(targets.len()) * (std::mem::size_of::<(Ex, Vec<u32>)>() + 1) + 16;
        let cap = eligible_bytes + map_bytes + peak_slots * std::mem::size_of::<u32>();
        let retained = map_bytes + retained_slots * std::mem::size_of::<u32>();
        let n_told = told.len();
        let pooled_bytes = Arc::new(AtomicUsize::new(0));
        let pooled = pooled_bytes.clone();
        let pool_limit = Limits {
            charge: Some(Arc::new(move |more| {
                pooled
                    .try_update(Ordering::Relaxed, Ordering::Relaxed, |paid| {
                        paid.checked_add(more).filter(|&total| total <= cap)
                    })
                    .is_ok()
            })),
            ..Limits::default()
        };
        let capped = |limit| Limits {
            max_memory: Some(limit),
            ..Limits::default()
        };
        for limits in [capped(cap - 1), capped(cap), pool_limit] {
            let n_nodes = it.nodes.len();
            let budget = Budget::new(&limits, 0, None);
            it.ranges.begin(budget.clone());
            let result = dt_fires(&mut it, &run, &b, &mut told);
            if limits.max_memory == Some(cap - 1) {
                assert!(matches!(result, Err(Halt::Memory(_))));
                assert_eq!(it.nodes.len(), n_nodes);
                assert_eq!(told.len(), n_told);
            } else {
                let (refire, guard) = result.expect("prior peak must fit");
                assert_eq!(budget.peak(), cap);
                assert_eq!(budget.retained_storage().1, retained);
                assert_eq!(guard.as_ref().expect("refire guard").bytes(), retained);
                let markers = &told[n_told..];
                assert_eq!(markers.len(), targets.len());
                for ((&target, &n), &(from, to)) in targets.iter().zip(distinct).zip(markers) {
                    let cs = refire.get(&target).expect("target refire");
                    let len = n + usize::from(!appends.is_empty());
                    assert!(
                        cs.iter()
                            .copied()
                            .eq(atoms[..len].iter().map(|&(c, _, _)| c))
                    );
                    assert_eq!(cs.capacity(), (2 * n).max(4));
                    assert_eq!(
                        (from, it.nodes[to as usize]),
                        (target, Node::DtFire(target))
                    );
                }
            }
            it.ranges.end();
            assert_eq!(budget.retained_storage().1, 0);
            told.truncate(n_told);
        }
        assert_eq!(pooled_bytes.load(Ordering::Relaxed), cap);
    }

    #[test]
    fn dt_fires_two_full_targets_fit_under_prior_peak() {
        use super::super::Limits;
        use super::super::dtcheck::{
            atoms::{DtKey, DtRun, Stop},
            comp::Comps,
            range::{Defs, Ranges as DtRanges, map_buckets_for},
        };
        use super::super::numeric::Budget;
        use super::{Builder, Ex, Interner, Node, RoleName, dt_fires};
        use crate::model::{AxiomKind, Class, ClassExpression, DataProperty, DataRange, Datatype};
        use crate::vocab;

        let props: Vec<_> = (0..4)
            .map(|i| DataProperty::new(format!("urn:dt:cap_p{i}")))
            .collect();
        let axioms: Vec<_> = props
            .iter()
            .map(|dp| {
                AxiomKind::SubClassOf(
                    ClassExpression::Class(Class::new("urn:dt:CapBody")),
                    ClassExpression::DataAllValuesFrom(
                        vec![dp.clone()],
                        DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
                    ),
                )
            })
            .collect();
        let refs: Vec<_> = axioms.iter().collect();
        let run = DtRun {
            comps: Comps::of(&refs, &Defs::default(), &|| false).expect("checker components"),
            defs: Defs::default(),
            stop: Stop::default(),
            budget: None,
            stopped: None,
        };
        let mut it = Interner::new();
        let targets = [
            it.class(&Class::new("urn:dt:CapA")),
            it.class(&Class::new("urn:dt:CapB")),
        ];
        let mut b = Builder::default();
        let mut told = Vec::new();
        // The fifth atom repeats the first component with a distinct cardinality.
        for (i, dp) in props.iter().chain(&props[..1]).enumerate() {
            let role = it.role(RoleName::Data(dp.clone()));
            let id = it.dt.id(DtKey {
                role,
                at_least: true,
                n: (i + 1) as u64,
                range: DtRanges::FULL,
            });
            let e = it.node(Node::DtAtom(id));
            b.dt_body.push(e);
            told.extend(targets.map(|t| (e, t)));
        }
        let eligible_bytes = b.dt_body.len() * std::mem::size_of::<(Ex, u32)>();
        let map_bytes =
            map_buckets_for(targets.len()) * (std::mem::size_of::<(Ex, Vec<u32>)>() + 1) + 16;
        // The prior implementation grew both vectors. The second growth admitted a new
        // capacity-8 vector while the first capacity-8 and second capacity-4 were live.
        let cap = eligible_bytes + map_bytes + (8 + 4 + 8) * std::mem::size_of::<u32>();
        let budget = Budget::new(
            &Limits {
                max_memory: Some(cap),
                ..Limits::default()
            },
            0,
            None,
        );
        it.ranges.begin(budget.clone());
        let n_told = told.len();
        let (refire, guard) = dt_fires(&mut it, &run, &b, &mut told).expect("prior peak must fit");
        let mut expected: Vec<_> = props
            .iter()
            .map(|dp| run.comps.component(dp).expect("component"))
            .collect();
        expected.sort_unstable();
        for t in targets {
            assert_eq!(refire.get(&t).expect("target refire"), &expected);
        }
        let markers = &told[n_told..];
        assert_eq!(markers.len(), targets.len());
        for (&(from, to), target) in markers.iter().zip(targets) {
            assert_eq!(from, target);
            assert_eq!(it.nodes[to as usize], Node::DtFire(target));
        }
        let refire_bytes = refire.table_bytes() + 8 * std::mem::size_of::<u32>();
        assert_eq!(budget.peak(), eligible_bytes + refire_bytes);
        assert_eq!(budget.retained_storage().1, refire_bytes);
        drop((refire, guard));
        it.ranges.end();
        assert_eq!(budget.retained_storage().1, 0);
    }

    #[test]
    fn dt_fires_full_duplicate_tail_does_not_grow_or_exceed_cap() {
        use super::super::Limits;
        use super::super::dtcheck::{
            atoms::{DtKey, DtRun, Stop},
            comp::Comps,
            range::{Defs, Ranges as DtRanges, map_buckets_for},
        };
        use super::super::numeric::Budget;
        use super::{
            Builder, Ex, Halt, Interner, Node, RoleName, dt_fires, dt_fires_compaction_stats,
            reset_dt_fires_compaction_stats,
        };
        use crate::model::{AxiomKind, Class, ClassExpression, DataProperty, DataRange, Datatype};
        use crate::vocab;

        // 1. Feed one target 1,024 distinct components in order (filling capacity 1,024),
        //    followed by a 1,000-edge alternating duplicate tail starting with component 0.
        //    The duplicate tail must not double capacity to 2,048, must not raise the
        //    512 + 1,024 overlap peak, must fit under the live-overlap cap, and must avoid
        //    quadratic re-compaction.
        let k = 1024usize;
        let tail = 1000usize;
        let props: Vec<DataProperty> = (0..k)
            .map(|i| DataProperty::new(format!("urn:dt:dup_p{i}")))
            .collect();
        let axioms: Vec<AxiomKind> = props
            .iter()
            .enumerate()
            .map(|(i, dp)| {
                AxiomKind::SubClassOf(
                    ClassExpression::Class(Class::new(format!("urn:dt:DUP_A{i}"))),
                    ClassExpression::DataAllValuesFrom(
                        vec![dp.clone()],
                        DataRange::Datatype(Datatype::new(vocab::XSD_INTEGER)),
                    ),
                )
            })
            .collect();
        let refs: Vec<&AxiomKind> = axioms.iter().collect();
        let comps = Comps::of(&refs, &Defs::default(), &|| false).expect("1024 comps");
        let mut it = Interner::new();
        let mut role_comps: Vec<(Role, u32)> = props
            .iter()
            .map(|dp| {
                let c = comps.component(dp).expect("checker comp");
                (it.role(RoleName::Data(dp.clone())), c)
            })
            .collect();
        role_comps.sort_unstable_by_key(|&(_, c)| c);
        let target = it.class(&Class::new("urn:dt:DupTarget"));
        let mut b = Builder::default();
        let mut atoms = Vec::with_capacity(k);
        let mut told_base: Vec<(Ex, Ex)> = Vec::with_capacity(k + tail);
        for (idx, &(role, _)) in role_comps.iter().enumerate() {
            let id = it.dt.id(DtKey {
                role,
                at_least: true,
                n: (idx + 1) as u64,
                range: DtRanges::FULL,
            });
            let e = it.node(Node::DtAtom(id));
            b.dt_body.push(e);
            atoms.push(e);
            told_base.push((e, target));
        }
        for j in 0..tail {
            told_base.push((atoms[j % 2], target));
        }
        let run = DtRun {
            comps,
            defs: Defs::default(),
            stop: Stop::default(),
            budget: None,
            stopped: None,
        };
        let expected: Vec<u32> = role_comps.iter().map(|&(_, c)| c).collect();
        let eligible_bytes = k * std::mem::size_of::<(Ex, u32)>();

        let uncap = Budget::new(&Limits::default(), 0, None);
        it.ranges.begin(uncap.clone());
        let mut told = told_base.clone();
        reset_dt_fires_compaction_stats();
        let (refire, guard) = dt_fires(&mut it, &run, &b, &mut told).expect("uncapped");
        let (compactions, inspected) = dt_fires_compaction_stats();
        let cs = refire.get(&target).expect("target refire");
        assert_eq!(cs.as_slice(), expected.as_slice());
        assert_eq!(
            cs.capacity(),
            k,
            "duplicate-only tail on full vec must preserve capacity {k}, got {}",
            cs.capacity()
        );
        let map_bytes = refire.table_bytes();
        let final_refire_bytes = map_bytes + k * std::mem::size_of::<u32>();
        assert_eq!(
            guard.as_ref().expect("guard").bytes(),
            final_refire_bytes,
            "carried refire guard must match un-doubled live backing"
        );
        assert_eq!(uncap.retained_storage().1, final_refire_bytes);
        let expected_live_peak =
            eligible_bytes + map_bytes + (k / 2 + k) * std::mem::size_of::<u32>();
        assert_eq!(
            uncap.peak(),
            expected_live_peak,
            "duplicate tail must not trigger a 1024 + 2048 growth overlap peak"
        );
        assert!(
            compactions <= 20,
            "expected bounded compactions (<= 20), got {compactions}"
        );
        assert!(
            inspected <= 4 * (k + tail),
            "expected linear inspected entries (<= {}), got {inspected}",
            4 * (k + tail)
        );
        drop((refire, guard));
        it.ranges.end();
        assert_eq!(uncap.retained_storage().1, 0);

        // Must succeed at the exact 512 + 1,024 live-overlap cap (strictly below both
        // cumulative historical charges and a spurious 1,024 + 2,048 duplicate-triggered growth).
        let live_cap = Budget::new(
            &Limits {
                max_memory: Some(expected_live_peak),
                ..Limits::default()
            },
            0,
            None,
        );
        it.ranges.begin(live_cap);
        let mut told = told_base.clone();
        let (refire_ok, guard_ok) =
            dt_fires(&mut it, &run, &b, &mut told).expect("live-peak cap must succeed");
        let cs_ok = refire_ok.get(&target).expect("target refire");
        assert_eq!(cs_ok.capacity(), k);
        assert_eq!(
            guard_ok.as_ref().expect("guard").bytes(),
            final_refire_bytes
        );
        drop((refire_ok, guard_ok));
        it.ranges.end();

        // One byte below the true 512 + 1,024 overlap peak must still refuse.
        let under_cap = Budget::new(
            &Limits {
                max_memory: Some(expected_live_peak - 1),
                ..Limits::default()
            },
            0,
            None,
        );
        it.ranges.begin(under_cap);
        let mut told = told_base;
        let err = dt_fires(&mut it, &run, &b, &mut told);
        it.ranges.end();
        assert!(
            matches!(err, Err(Halt::Memory(_))),
            "expected Halt::Memory one byte below live overlap peak, got {:?}",
            err.err()
        );

        // 2. Exercise both full targets at the prior peak for real growth. A second new
        //    component on t0 also checks that its first append cleared sorted mode.
        let m = 128usize;
        let mut it2 = Interner::new();
        let t0 = it2.class(&Class::new("urn:dt:MultiDupT0"));
        let t1 = it2.class(&Class::new("urn:dt:MultiDupT1"));
        let mut b2 = Builder::default();
        let mut atoms2 = Vec::with_capacity(m + 2);
        for (idx, dp) in props.iter().take(m + 2).enumerate() {
            let role = it2.role(RoleName::Data(dp.clone()));
            let id = it2.dt.id(DtKey {
                role,
                at_least: true,
                n: (idx + 1) as u64,
                range: DtRanges::FULL,
            });
            let e = it2.node(Node::DtAtom(id));
            b2.dt_body.push(e);
            atoms2.push((e, run.comps.component(dp).expect("comp")));
        }
        atoms2.sort_unstable_by_key(|&(_, c)| c);
        let mut told2 = Vec::new();
        for &(e, _) in atoms2.iter().take(m) {
            told2.push((e, t0));
            told2.push((e, t1));
        }
        // Interleaved alternating duplicates across both full targets.
        for j in 0..500usize {
            let e = atoms2[j % 2].0;
            told2.push((e, t0));
            told2.push((e, t1));
        }
        // Append the two new components in descending order on t0.
        told2.push((atoms2[m + 1].0, t0));
        told2.push((atoms2[m].0, t0));
        told2.push((atoms2[m].0, t1));
        for j in 0..500usize {
            let e = atoms2[j % 2].0;
            told2.push((e, t0));
            told2.push((e, t1));
        }
        let eligible2_bytes = b2.dt_body.len() * std::mem::size_of::<(Ex, u32)>();
        let map2_bytes = map_buckets_for(2) * (std::mem::size_of::<(Ex, Vec<u32>)>() + 1) + 16;
        let peak2 = eligible2_bytes + map2_bytes + 5 * m * std::mem::size_of::<u32>();
        let cap2 = Budget::new(
            &Limits {
                max_memory: Some(peak2),
                ..Limits::default()
            },
            0,
            None,
        );
        it2.ranges.begin(cap2.clone());
        reset_dt_fires_compaction_stats();
        let total_edges = told2.len();
        let (refire2, guard2) = dt_fires(&mut it2, &run, &b2, &mut told2).expect("multi-target");
        let (compactions2, inspected2) = dt_fires_compaction_stats();
        let cs0 = refire2.get(&t0).expect("t0 refire");
        let cs1 = refire2.get(&t1).expect("t1 refire");
        let expected0: Vec<u32> = atoms2.iter().map(|&(_, c)| c).collect();
        let expected1: Vec<u32> = atoms2.iter().take(m + 1).map(|&(_, c)| c).collect();
        assert_eq!(cs0.as_slice(), expected0.as_slice());
        assert_eq!(cs1.as_slice(), expected1.as_slice());
        assert_eq!(cs0.capacity(), 2 * m);
        assert_eq!(cs1.capacity(), 2 * m);
        let expected_refire2_bytes = map2_bytes + 4 * m * std::mem::size_of::<u32>();
        assert_eq!(
            guard2.as_ref().expect("guard2").bytes(),
            expected_refire2_bytes,
            "refire guard must match live backing"
        );
        assert_eq!(cap2.retained_storage().1, expected_refire2_bytes);
        assert_eq!(cap2.peak(), peak2);
        assert!(
            compactions2 <= 30,
            "expected bounded multi-target compactions (<= 30), got {compactions2}"
        );
        assert!(
            inspected2 <= 4 * total_edges,
            "expected linear multi-target inspected entries (<= {}), got {inspected2}",
            4 * total_edges
        );
        drop((refire2, guard2));
        it2.ranges.end();
        assert_eq!(cap2.retained_storage().1, 0);
    }
}
