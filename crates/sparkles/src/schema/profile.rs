//! Per-class property profiles: for each class, the predicates its instances use and the
//! predicates that point at them, with counts.
//!
//! The instances of a class are the subjects typed with it in the selection, as the
//! report counts them ([`ClassObserved::instances`](super::ClassObserved)): direct types
//! only, so subclass instances count under a superclass only when the selection holds
//! materialized inferences. A subject with several classes counts under each.
//!
//! The profile reads the `(subject, class)` pairs of `rdf:type` once, sorted by subject,
//! and then per predicate one pass over `POS[p]`, which classifies literal objects in
//! sorted vocabulary batches and counts the incoming arcs of each class, and one pass over
//! `PSO[p]`, which joins each subject's run of objects with its classes and the classes
//! of its objects.

use super::{
    Budget, Iris, Kind, SchemaError, SchemaOptions, Selected, SnapshotInfo, Src, in_phase, is_iri,
    literal_suffix, quick_kind, snapshot_identity, subject_types,
};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::store::Snapshot;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Version of the JSON shape of [`ClassProfiles`].
pub const PROFILE_FORMAT: u32 = 1;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
const RDF_DIR_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#dirLangString";

/// Parameters of [`profiles`].
#[derive(Clone, Debug, Default)]
pub struct ProfileOptions {
    /// The selection, budgets and graph view. `declared_graph`, `declared_from_inferred`,
    /// `term_totals` and `subject_classes` are not read.
    pub schema: SchemaOptions,
    /// Profile only these classes (IRIs); empty: every class with instances.
    pub classes: Vec<String>,
}

/// Triples of one kind of object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileKinds {
    #[serde(skip_serializing_if = "is_zero")]
    pub iri: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub blank: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub triple_term: u64,
    /// Literal triples by datatype IRI, sorted by datatype.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub literals: Vec<DatatypeCount>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DatatypeCount {
    pub datatype: String,
    pub triples: u64,
}

/// Triples whose object is an instance of one class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClassCount {
    pub class: String,
    pub triples: u64,
}

/// One predicate the instances of a class use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyProfile {
    pub predicate: String,
    /// Instances with at least one value.
    pub instances: u64,
    /// Distinct triples of those instances.
    pub triples: u64,
    /// The fewest and the most distinct values of one instance that has any: a
    /// measurement of this snapshot, not a constraint.
    pub min_per_instance: u64,
    pub max_per_instance: u64,
    pub objects: ProfileKinds,
    /// The classes of the IRI and blank-node values, by triples, most first (then by
    /// class IRI). A value with several classes counts under each.
    pub object_classes: Vec<ClassCount>,
}

/// One predicate whose values include instances of a class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IncomingProfile {
    pub predicate: String,
    /// Distinct triples whose object is an instance.
    pub triples: u64,
    /// Instances that are the object of at least one of them.
    pub instances: u64,
}

/// The profile of one class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClassProfile {
    pub class: String,
    pub builtin: bool,
    /// Subjects typed with the class in the selection.
    pub instances: u64,
    /// Most used first, then by predicate IRI.
    pub properties: Vec<PropertyProfile>,
    /// Most triples first, then by predicate IRI.
    pub incoming: Vec<IncomingProfile>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSelection {
    pub graph: String,
    pub reasoning: bool,
}

/// The profiles of the classes of one snapshot, sorted by class IRI.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassProfiles {
    pub profile_format: u32,
    pub snapshot: SnapshotInfo,
    pub selection: ProfileSelection,
    pub classes: Vec<ClassProfile>,
}

/// One predicate's profile over the instances of one class, by ids.
#[derive(Default)]
struct Acc {
    instances: u64,
    triples: u64,
    min: u64,
    max: u64,
    iri: u64,
    blank: u64,
    triple: u64,
    /// interned literal key suffix → triples
    lits: FxHashMap<u32, u64>,
    object_classes: FxHashMap<u64, u64>,
}

/// Incoming arcs of one predicate into the instances of one class.
#[derive(Default)]
struct In {
    triples: u64,
    instances: u64,
}

/// Literal key suffixes, interned.
#[derive(Default)]
struct Suffixes {
    names: Vec<Vec<u8>>,
    index: FxHashMap<Vec<u8>, u32>,
}

impl Suffixes {
    fn get(&mut self, s: &[u8]) -> u32 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.names.len() as u32;
        self.names.push(s.to_vec());
        self.index.insert(s.to_vec(), i);
        i
    }

    /// The datatype IRI of a suffix: `xsd:string` for none, `rdf:langString` or
    /// `rdf:dirLangString` for a language tag.
    fn datatype(&self, i: u32) -> String {
        let s = &self.names[i as usize];
        match s.first() {
            None => XSD_STRING.to_string(),
            Some(b'@') if s.ends_with(b"--ltr") || s.ends_with(b"--rtl") => {
                RDF_DIR_LANG_STRING.to_string()
            }
            Some(b'@') => RDF_LANG_STRING.to_string(),
            Some(_) => String::from_utf8_lossy(&s[1..]).into_owned(),
        }
    }
}

/// What an object is: a kind, or a literal with its interned suffix.
#[derive(Clone, Copy)]
enum Obj {
    Iri,
    Blank,
    Triple,
    Literal(u32),
    Other,
}

fn obj_of(kind: Kind, sfx: &mut Suffixes) -> Obj {
    match kind {
        Kind::Iri => Obj::Iri,
        Kind::Blank => Obj::Blank,
        Kind::Triple => Obj::Triple,
        Kind::Literal(s) => Obj::Literal(sfx.get(&s)),
        Kind::Other => Obj::Other,
    }
}

/// The classes of `x` in `types` (sorted by subject).
fn classes_of(types: &[(u64, u64)], x: u64) -> &[(u64, u64)] {
    let start = types.partition_point(|t| t.0 < x);
    let end = start + types[start..].partition_point(|t| t.0 == x);
    &types[start..end]
}

/// Compute the profiles of the classes of `snap`.
pub fn profiles(snap: &Arc<Snapshot>, opts: &ProfileOptions) -> Result<ClassProfiles, SchemaError> {
    let snap: &Snapshot = snap;
    let so = SchemaOptions {
        declared_graph: None,
        subject_classes: false,
        term_totals: false,
        ..opts.schema.clone()
    };
    let budget = Budget {
        deadline: so.deadline,
        cancel: so.cancel.as_deref(),
    };
    in_phase(budget.check(), || "starting".into())?;
    let sel = Selected::resolve(snap, &so)?;
    let filter = &sel.observed;
    let src = in_phase(Src::for_filters(snap, &[filter], &budget), || {
        "reading the selected graphs".into()
    })?;
    let rdf_type = snap.lookup_iri(RDF_TYPE).map(|i| i.0);
    let types = match rdf_type {
        Some(t) => in_phase(subject_types(&src, t, filter, &budget), || {
            "reading rdf:type".into()
        })?,
        None => Vec::new(),
    };
    let mut instances: FxHashMap<u64, u64> = FxHashMap::default();
    for (_, c) in &types {
        *instances.entry(*c).or_default() += 1;
    }
    if instances.len() > so.max_entries {
        return Err(SchemaError::TooManyEntries {
            kind: "classes",
            count: instances.len(),
            limit: so.max_entries,
        });
    }
    // the classes to profile; a requested class without instances gets an empty profile
    let mut iris = Iris {
        snap,
        cache: FxHashMap::default(),
    };
    let mut wanted: FxHashSet<u64> = FxHashSet::default();
    let mut missing: Vec<String> = Vec::new();
    if opts.classes.is_empty() {
        wanted.extend(instances.keys().copied());
    } else {
        for c in &opts.classes {
            match snap.lookup_iri(c).map(|i| i.0) {
                Some(id) if instances.contains_key(&id) => {
                    wanted.insert(id);
                }
                _ => missing.push(c.clone()),
            }
        }
    }

    let preds = in_phase(src.distinct_first(Perm::Pso), || {
        "listing predicates".into()
    })?;
    let mut sfx = Suffixes::default();
    let mut out: FxHashMap<u64, Vec<(u64, Acc)>> = FxHashMap::default();
    let mut incoming: FxHashMap<u64, Vec<(u64, In)>> = FxHashMap::default();
    let vocab = &snap.generation.vocab;
    for (i, &p) in preds.iter().enumerate() {
        if Some(p) == rdf_type || !is_iri(snap, p) || wanted.is_empty() {
            continue;
        }
        let phase = || format!("profiling predicates ({}/{})", i + 1, preds.len());
        // POS: incoming arcs per class of the object, and the base-vocabulary literals
        let mut base_lits: Vec<u64> = Vec::new();
        let mut ins: FxHashMap<u64, In> = FxHashMap::default();
        let mut prev: Option<(u64, u64)> = None;
        let mut run: Option<(u64, u64)> = None;
        let mut end_object = |o: u64, n: u64, base_lits: &mut Vec<u64>| {
            let id = Id(o);
            if id.tag() == Tag::Vocab
                && !vocab.is_iri(id.payload())
                && !vocab.is_triple(id.payload())
            {
                base_lits.push(id.payload());
                return;
            }
            for (_, c) in classes_of(&types, o) {
                if wanted.contains(c) {
                    let e = ins.entry(*c).or_default();
                    e.triples += n;
                    e.instances += 1;
                }
            }
        };
        in_phase(
            src.for_each_key(Perm::Pos, &[p], &budget, |k| {
                if !filter.accepts(k[3]) || prev == Some((k[1], k[2])) {
                    return;
                }
                prev = Some((k[1], k[2]));
                match &mut run {
                    Some((o, n)) if *o == k[1] => *n += 1,
                    _ => {
                        if let Some((o, n)) = run {
                            end_object(o, n, &mut base_lits);
                        }
                        run = Some((k[1], 1));
                    }
                }
            }),
            phase,
        )?;
        if let Some((o, n)) = run {
            end_object(o, n, &mut base_lits);
        }
        let mut lit_of: FxHashMap<u64, u32> = FxHashMap::default();
        for chunk in base_lits.chunks(4096) {
            in_phase(budget.check(), phase)?;
            vocab.get_sorted(chunk, |payload, key| {
                lit_of.insert(Id::vocab(payload).0, sfx.get(literal_suffix(key)));
            });
        }
        // PSO: each subject's run of objects, under each of its wanted classes
        let mut accs: FxHashMap<u64, Acc> = FxHashMap::default();
        let mut subject: Option<u64> = None;
        let mut objs: Vec<u64> = Vec::new();
        let mut pos = 0usize;
        let mut flush = |s: u64, objs: &[u64], pos: &mut usize, sfx: &mut Suffixes| {
            *pos += types[*pos..].partition_point(|t| t.0 < s);
            let mut end = *pos;
            while types.get(end).is_some_and(|t| t.0 == s) {
                end += 1;
            }
            let cs: Vec<u64> = types[*pos..end]
                .iter()
                .map(|t| t.1)
                .filter(|c| wanted.contains(c))
                .collect();
            if cs.is_empty() {
                return;
            }
            let n = objs.len() as u64;
            let kinds: Vec<Obj> = objs
                .iter()
                .map(|&o| match lit_of.get(&o) {
                    Some(&l) => Obj::Literal(l),
                    None => quick_kind(snap, o).map_or(Obj::Other, |k| obj_of(k, sfx)),
                })
                .collect();
            for c in cs {
                let a = accs.entry(c).or_default();
                a.min = if a.instances == 0 { n } else { a.min.min(n) };
                a.max = a.max.max(n);
                a.instances += 1;
                a.triples += n;
                for (&o, k) in objs.iter().zip(&kinds) {
                    match k {
                        Obj::Iri => a.iri += 1,
                        Obj::Blank => a.blank += 1,
                        Obj::Triple => a.triple += 1,
                        Obj::Literal(l) => *a.lits.entry(*l).or_default() += 1,
                        Obj::Other => {}
                    }
                    if matches!(k, Obj::Iri | Obj::Blank) {
                        for (_, oc) in classes_of(&types, o) {
                            *a.object_classes.entry(*oc).or_default() += 1;
                        }
                    }
                }
            }
        };
        let mut prev: Option<(u64, u64)> = None;
        in_phase(
            src.for_each_key(Perm::Pso, &[p], &budget, |k| {
                if !filter.accepts(k[3]) || prev == Some((k[1], k[2])) {
                    return;
                }
                prev = Some((k[1], k[2]));
                if subject != Some(k[1]) {
                    if let Some(s) = subject {
                        flush(s, &objs, &mut pos, &mut sfx);
                    }
                    subject = Some(k[1]);
                    objs.clear();
                }
                objs.push(k[2]);
            }),
            phase,
        )?;
        if let Some(s) = subject {
            flush(s, &objs, &mut pos, &mut sfx);
        }
        for (c, a) in accs {
            out.entry(c).or_default().push((p, a));
        }
        for (c, e) in ins {
            incoming.entry(c).or_default().push((p, e));
        }
    }

    // assembly, by IRI
    let mut classes: Vec<ClassProfile> = Vec::new();
    for c in wanted {
        let Some(class) = iris.get(c) else { continue };
        let mut properties: Vec<PropertyProfile> = Vec::new();
        for (p, a) in out.remove(&c).unwrap_or_default() {
            let Some(predicate) = iris.get(p) else {
                continue;
            };
            let mut lits: BTreeMap<String, u64> = BTreeMap::new();
            for (l, n) in a.lits {
                *lits.entry(sfx.datatype(l)).or_default() += n;
            }
            let mut object_classes: Vec<ClassCount> = a
                .object_classes
                .into_iter()
                .filter_map(|(k, n)| {
                    Some(ClassCount {
                        class: iris.get(k)?,
                        triples: n,
                    })
                })
                .collect();
            object_classes.sort_by(|x, y| y.triples.cmp(&x.triples).then(x.class.cmp(&y.class)));
            properties.push(PropertyProfile {
                predicate,
                instances: a.instances,
                triples: a.triples,
                min_per_instance: a.min,
                max_per_instance: a.max,
                objects: ProfileKinds {
                    iri: a.iri,
                    blank: a.blank,
                    triple_term: a.triple,
                    literals: lits
                        .into_iter()
                        .map(|(datatype, triples)| DatatypeCount { datatype, triples })
                        .collect(),
                },
                object_classes,
            });
        }
        properties.sort_by(|x, y| {
            y.instances
                .cmp(&x.instances)
                .then_with(|| x.predicate.cmp(&y.predicate))
        });
        let mut inc: Vec<IncomingProfile> = incoming
            .remove(&c)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(p, e)| {
                Some(IncomingProfile {
                    predicate: iris.get(p)?,
                    triples: e.triples,
                    instances: e.instances,
                })
            })
            .collect();
        inc.sort_by(|x, y| {
            y.triples
                .cmp(&x.triples)
                .then_with(|| x.predicate.cmp(&y.predicate))
        });
        classes.push(ClassProfile {
            builtin: super::builtin(&class),
            class,
            instances: instances.get(&c).copied().unwrap_or(0),
            properties,
            incoming: inc,
        });
    }
    for class in missing {
        classes.push(ClassProfile {
            builtin: super::builtin(&class),
            class,
            instances: 0,
            properties: Vec::new(),
            incoming: Vec::new(),
        });
    }
    classes.sort_by(|a, b| a.class.cmp(&b.class));
    classes.dedup_by(|a, b| a.class == b.class);
    Ok(ClassProfiles {
        profile_format: PROFILE_FORMAT,
        snapshot: SnapshotInfo {
            version: snapshot_identity(snap),
            commit: snap.commit,
            generation: snap.generation.name.clone(),
            computed_at: crate::builder::now_rfc3339(),
        },
        selection: ProfileSelection {
            graph: so.graph.name().to_string(),
            reasoning: so.include_inferred,
        },
        classes,
    })
}
