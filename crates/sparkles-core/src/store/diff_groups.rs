//! A diff grouped by class ([`Diff::group_by_class`]): the counts of its changes by the
//! class of the subject, the predicate, and the class or kind of the object, without the
//! quads (docs/specs/U01-time-slider.md, Phase 4).
//!
//! The class of a resource is read at the `to` state for an added quad and at the `from`
//! state for a removed one, from the `rdf:type` triples of the graphs the caller may read,
//! in the state as the caller's view reads it, so a hidden type triple leaves a resource
//! without a class rather than revealing it. Each state is read once: the distinct
//! resources of the changes on that side are looked up together, either by one seek per
//! resource or, when they are many next to the type triples, by one scan of them. A
//! resource with several classes counts under the one most of the resources looked up in
//! that state share, the smallest IRI first on a tie.

use super::*;
use crate::access::GraphAccess;
use rustc_hash::{FxHashMap, FxHashSet};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// The most groups an answer lists.
pub const MAX_GROUPS: usize = 5_000;

/// What the grouping holds for each distinct resource, each class and each group, in
/// bytes, as charged to the request's memory: a hash-table slot with its key and value,
/// rounded up.
const RESOURCE_BYTES: u64 = 64;
const CLASS_BYTES: u64 = 48;
const GROUP_BYTES: u64 = 96;
/// What the diff holds for each change it keeps: four shared term keys and an operation.
pub const CHANGE_BYTES: u64 = 96;

/// The object side of a group.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectGroup {
    /// A resource, by its class, or `None` for a blank node or triple term without one.
    /// The object of an `rdf:type` change is the class it names.
    Class(Option<String>),
    /// A literal, by its datatype IRI.
    Literal(String),
    /// An IRI without a class at that state.
    Iri,
}

/// One group of changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassGroup {
    pub subject_class: Option<String>,
    pub predicate: String,
    pub object: ObjectGroup,
    pub added: u64,
    pub removed: u64,
}

/// The grouped changes, the largest groups first.
#[derive(Clone, Debug, Default)]
pub struct ClassGroups {
    pub groups: Vec<ClassGroup>,
    /// the groups found, of which `groups` lists at most the limit asked for
    pub total: usize,
    pub truncated: bool,
}

/// A group while it is counted: keys shared with the diff.
#[derive(Clone, PartialEq, Eq, Hash)]
enum ObjKey {
    Class(Option<Arc<[u8]>>),
    Literal(String),
    Iri,
}

type GroupKey = (Option<Arc<[u8]>>, Arc<[u8]>, ObjKey);

/// The chosen class of each resource, by their keys.
type ClassOf = FxHashMap<Arc<[u8]>, Arc<[u8]>>;

fn iri_of(key: &[u8]) -> String {
    match crate::id::key_to_term(key) {
        Term::NamedNode(n) => n.into_string(),
        t => t.to_string(),
    }
}

/// The classes of `resources` in `snap`, each resource's chosen class by key.
fn classes_at(
    snap: &Arc<Snapshot>,
    view: Option<&Arc<GraphAccess>>,
    resources: &FxHashSet<Arc<[u8]>>,
    o: &DiffOptions,
    charge: &mut dyn FnMut(u64) -> Result<()>,
) -> Result<ClassOf> {
    let mut out = FxHashMap::default();
    if resources.is_empty() {
        return Ok(out);
    }
    // the state as the view reads it: guards decided, hidden triples taken out
    let view = view
        .filter(|v| !v.reads_everything())
        .map(|v| v.bound(snap));
    let snap = match &view {
        Some(v) => v.masked(snap)?,
        None => snap.clone(),
    };
    let Some(ty) = snap.lookup_iri(RDF_TYPE) else {
        return Ok(out);
    };
    // the resources this state knows, by id
    let mut ids: FxHashMap<u64, Arc<[u8]>> = FxHashMap::default();
    for k in resources {
        if let Some(id) = key_id(&snap, k)
            && matches!(snap.term_kind(id), TermKind::Iri | TermKind::BNode)
        {
            ids.insert(id.0, k.clone());
        }
    }
    let mut readable: FxHashMap<u64, bool> = FxHashMap::default();
    let mut graph_ok = |g: u64| -> bool {
        match &view {
            None => true,
            Some(v) if v.reads_all() => true,
            Some(v) => *readable
                .entry(g)
                .or_insert_with(|| v.readable_id(&snap, Id(g))),
        }
    };
    // the classes of each resource, by id
    let mut found: FxHashMap<u64, Vec<u64>> = FxHashMap::default();
    let mut add = |s: u64, c: u64, charge: &mut dyn FnMut(u64) -> Result<()>| -> Result<()> {
        let e = found.entry(s).or_default();
        if !e.contains(&c) {
            e.push(c);
            charge(16)?;
        }
        Ok(())
    };
    let mut seen = 0u64;
    let typed = snap.count(Perm::Pso, &[ty.0])?;
    let mut err: Option<Error> = None;
    if (ids.len() as u64).saturating_mul(16) >= typed {
        // many resources next to the type triples: one scan of them
        snap.scan(Perm::Pso, &[ty.0], |c| {
            let mut visit = |k: &Key| -> Result<()> {
                seen += 1;
                if seen & 0xFFF == 0 {
                    o.check()?;
                }
                let q = Perm::Pso.to_quad(k);
                if ids.contains_key(&q[0].0)
                    && snap.term_kind(q[2]) == TermKind::Iri
                    && graph_ok(q[3].0)
                {
                    add(q[0].0, q[2].0, charge)?;
                }
                Ok(())
            };
            let r = match c {
                Chunk::Block(b, s, e) => (s..e).try_for_each(|i| visit(&b.key(i))),
                Chunk::Row(k) => visit(&k),
            };
            match r {
                Ok(()) => Ok(true),
                Err(e) => {
                    err = Some(e);
                    Ok(false)
                }
            }
        })?;
    } else {
        // one seek per resource
        let mut sorted: Vec<u64> = ids.keys().copied().collect();
        sorted.sort_unstable();
        for s in sorted {
            seen += 1;
            if seen & 0xFF == 0 {
                o.check()?;
            }
            snap.scan(Perm::Spo, &[s, ty.0], |c| {
                let mut visit = |k: &Key| -> Result<()> {
                    let q = Perm::Spo.to_quad(k);
                    if snap.term_kind(q[2]) == TermKind::Iri && graph_ok(q[3].0) {
                        add(s, q[2].0, charge)?;
                    }
                    Ok(())
                };
                let r = match c {
                    Chunk::Block(b, st, e) => (st..e).try_for_each(|i| visit(&b.key(i))),
                    Chunk::Row(k) => visit(&k),
                };
                match r {
                    Ok(()) => Ok(true),
                    Err(e) => {
                        err = Some(e);
                        Ok(false)
                    }
                }
            })?;
            if err.is_some() {
                break;
            }
        }
    }
    if let Some(e) = err {
        return Err(e);
    }
    o.check()?;
    // how many of the resources share each class, and each class's key
    let mut freq: FxHashMap<u64, u64> = FxHashMap::default();
    for cs in found.values() {
        for c in cs {
            *freq.entry(*c).or_default() += 1;
        }
    }
    let mut keys: FxHashMap<u64, Arc<[u8]>> = FxHashMap::default();
    for c in freq.keys() {
        if let Some(k) = snap.key(Id(*c)) {
            keys.insert(*c, Arc::from(&*k));
            charge(CLASS_BYTES + k.len() as u64)?;
        }
    }
    charge(found.len() as u64 * RESOURCE_BYTES)?;
    for (s, cs) in found {
        let best = cs
            .iter()
            .filter_map(|c| keys.get(c).map(|k| (freq[c], k)))
            .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(a.1)));
        if let (Some((_, k)), Some(r)) = (best, ids.get(&s)) {
            out.insert(r.clone(), k.clone());
        }
    }
    Ok(out)
}

impl Diff {
    /// The changes grouped by the class of the subject, the predicate and the class or
    /// kind of the object (see the module documentation). `state` gives the snapshot of
    /// one side, `Add` for `to` and `Remove` for `from`, and is asked only for a side that
    /// has changes. `o.graphs` is the caller's view, and `o`'s deadline and cancel flag
    /// apply. `charge` is told the bytes the grouping holds, and fails it when they pass a
    /// budget. At most `limit` groups are listed, the largest first.
    pub fn group_by_class(
        &self,
        state: &mut dyn FnMut(DiffOp) -> Result<Arc<Snapshot>>,
        o: &DiffOptions,
        limit: usize,
        charge: &mut dyn FnMut(u64) -> Result<()>,
    ) -> Result<ClassGroups> {
        let limit = limit.min(MAX_GROUPS);
        let type_key: Arc<[u8]> = Arc::from(crate::id::iri_key(RDF_TYPE));
        // the resources of each side, as subjects and as objects
        let mut sides: [FxHashSet<Arc<[u8]>>; 2] = Default::default();
        for (i, (op, k)) in self.keys().enumerate() {
            if i & 0xFFF == 0 {
                o.check()?;
            }
            let set = &mut sides[(op == DiffOp::Add) as usize];
            if set.insert(k[1].clone()) {
                charge(RESOURCE_BYTES)?;
            }
            let obj = &k[3];
            if *k[2] != *type_key && obj.first() != Some(&b'"') && set.insert(obj.clone()) {
                charge(RESOURCE_BYTES)?;
            }
        }
        let view = o.graphs.as_ref();
        let removed = if sides[0].is_empty() {
            FxHashMap::default()
        } else {
            classes_at(&state(DiffOp::Remove)?, view, &sides[0], o, charge)?
        };
        let added = if sides[1].is_empty() {
            FxHashMap::default()
        } else {
            classes_at(&state(DiffOp::Add)?, view, &sides[1], o, charge)?
        };
        drop(sides);
        let mut groups: FxHashMap<GroupKey, (u64, u64)> = FxHashMap::default();
        for (i, (op, k)) in self.keys().enumerate() {
            if i & 0xFFF == 0 {
                o.check()?;
            }
            let classes = if op == DiffOp::Add { &added } else { &removed };
            let obj = &k[3];
            let object = match obj.first() {
                Some(b'"') => match crate::id::key_to_term(obj) {
                    Term::Literal(l) => ObjKey::Literal(l.datatype().as_str().to_string()),
                    _ => ObjKey::Class(None),
                },
                _ if *k[2] == *type_key => match crate::id::key_to_term(obj) {
                    Term::NamedNode(_) => ObjKey::Class(Some(obj.clone())),
                    _ => ObjKey::Class(None),
                },
                _ => match classes.get(obj) {
                    Some(c) => ObjKey::Class(Some(c.clone())),
                    None if matches!(crate::id::key_to_term(obj), Term::NamedNode(_)) => {
                        ObjKey::Iri
                    }
                    None => ObjKey::Class(None),
                },
            };
            let key = (classes.get(&k[1]).cloned(), k[2].clone(), object);
            if !groups.contains_key(&key) {
                charge(GROUP_BYTES)?;
            }
            let e = groups.entry(key).or_default();
            match op {
                DiffOp::Add => e.0 += 1,
                DiffOp::Remove => e.1 += 1,
            }
        }
        let total = groups.len();
        let mut list: Vec<ClassGroup> = groups
            .into_iter()
            .map(|((s, p, ob), (a, r))| ClassGroup {
                subject_class: s.as_deref().map(iri_of),
                predicate: iri_of(&p),
                object: match ob {
                    ObjKey::Class(c) => ObjectGroup::Class(c.as_deref().map(iri_of)),
                    ObjKey::Literal(d) => ObjectGroup::Literal(d),
                    ObjKey::Iri => ObjectGroup::Iri,
                },
                added: a,
                removed: r,
            })
            .collect();
        list.sort_by(|a, b| {
            (b.added + b.removed)
                .cmp(&(a.added + a.removed))
                .then_with(|| a.subject_class.cmp(&b.subject_class))
                .then_with(|| a.predicate.cmp(&b.predicate))
                .then_with(|| a.object.cmp(&b.object))
        });
        list.truncate(limit);
        Ok(ClassGroups {
            truncated: total > list.len(),
            total,
            groups: list,
        })
    }
}
