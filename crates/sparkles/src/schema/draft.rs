//! Shapes drafted from the data: per-class property profiles over one snapshot, turned
//! into SHACL node shapes and a ShEx schema with a support threshold per constraint.
//!
//! The instances of a class are its SHACL instances in the selection: subjects with an
//! `rdf:type` that is the class or reaches it over `rdfs:subClassOf`. For each class and
//! predicate, the profile counts the instances with a value, the number of values per
//! instance, and per instance whether all its values share a node kind, a datatype, a
//! class, a closed set of values or a set of language tags. A constraint is drafted when
//! the share of the instances it applies to that satisfy it reaches the support, so at
//! support 1 the data conforms to the draft. `sh:minCount` applies to every instance;
//! the other constraints apply to the instances that have a value.
//!
//! Counting uses the permutation indexes, as [`discover`](super::discover) does: one pass
//! over `PSO[rdf:type]`, one over `PSO[rdfs:subClassOf]`, and per predicate one pass over
//! `POS[p]` (to classify literal objects in sorted batches) and one over `PSO[p]`.

use super::{
    Budget, GraphFilter, Iris, SchemaError, SchemaOptions, SnapshotInfo, Src, builtin, in_phase,
    is_iri, literal_suffix, resolve, snapshot_identity, within_view,
};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::store::Snapshot;
use oxrdf::Term;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Version of the JSON shape of [`ShapesDraft`].
pub const DRAFT_FORMAT: u32 = 1;

/// Default largest `sh:in` list.
pub const DEFAULT_MAX_IN: usize = 10;

/// Default largest `sh:maxCount` drafted.
pub const DEFAULT_MAX_COUNT: u64 = 1;

/// Distinct values (and language tags) tracked per class and predicate for `sh:in` and
/// `sh:languageIn`; also the largest `max_in` accepted.
pub const TRACKED_VALUES: usize = 64;

/// Distinct sets of values per instance tracked per class and predicate.
const TRACKED_SETS: usize = 4096;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const RDFS_SUBCLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const RDF_LANG_STRING: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#langString";
const XSD_BOOLEAN: &str = "http://www.w3.org/2001/XMLSchema#boolean";
const SH: &str = "http://www.w3.org/ns/shacl#";

/// The default namespace of shape IRIs: `urn:x-sparkles:shape:<dataset>:`, with the name
/// percent-encoded.
pub fn default_base(dataset: &str) -> String {
    let mut name = String::with_capacity(dataset.len());
    for b in dataset.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            name.push(b as char);
        } else {
            name.push_str(&format!("%{b:02X}"));
        }
    }
    format!("urn:x-sparkles:shape:{name}:")
}

/// Parameters of [`draft_shapes`].
#[derive(Clone, Debug)]
pub struct DraftOptions {
    /// The selection, budgets and graph view. Only `graph`, `inferred_graph`,
    /// `include_inferred`, `deadline`, `cancel`, `max_entries` and `graphs` are read.
    pub schema: SchemaOptions,
    /// The dataset name, for the header comments.
    pub dataset: String,
    /// Draft a constraint when at least this share of the instances it applies to
    /// satisfy it; in (0, 1].
    pub support: f64,
    /// Draft only these classes (IRIs); empty: every class with instances outside the
    /// built-in namespaces.
    pub classes: Vec<String>,
    /// Skip classes with fewer instances.
    pub min_instances: u64,
    /// The largest `sh:in` list (0: none); at most [`TRACKED_VALUES`].
    pub max_in: usize,
    /// The largest `sh:maxCount` drafted (0: none).
    pub max_count: u64,
    /// Draft closed shapes (`sh:closed`, ShEx `CLOSED`).
    pub closed: bool,
    /// The namespace of the shape IRIs (see [`default_base`]).
    pub base: String,
    /// Prefixes for the Turtle and ShExC texts.
    pub prefixes: Vec<(String, String)>,
}

impl Default for DraftOptions {
    fn default() -> Self {
        DraftOptions {
            schema: SchemaOptions {
                include_inferred: false,
                ..Default::default()
            },
            dataset: "data".into(),
            support: 1.0,
            classes: Vec::new(),
            min_instances: 1,
            max_in: DEFAULT_MAX_IN,
            max_count: DEFAULT_MAX_COUNT,
            closed: false,
            base: default_base("data"),
            prefixes: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------- output ------

/// One constraint, drafted or rejected, with the instances it applies to, those that
/// satisfy it and those it excludes.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintDraft {
    /// `minCount`, `maxCount`, `nodeKind`, `datatype`, `class`, `in`, `languageIn` or
    /// `uniqueLang` (the SHACL constraint component without its namespace and suffix).
    pub component: &'static str,
    pub value: ConstraintValue,
    pub applicable: u64,
    pub satisfied: u64,
    pub excluded: u64,
}

/// The value of a constraint: a count, an IRI, a list of terms in N-Triples syntax or of
/// language tags, or a boolean.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum ConstraintValue {
    Count(u64),
    Iri(String),
    List(Vec<String>),
    Bool(bool),
}

/// A property shape: one predicate of a class.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PropertyDraft {
    pub path: String,
    /// Instances of the class with at least one value.
    pub instances: u64,
    /// The largest number of distinct values of one instance.
    pub max_values: u64,
    pub constraints: Vec<ConstraintDraft>,
    /// The best candidates that missed the support or a limit.
    pub rejected: Vec<ConstraintDraft>,
}

/// A node shape: one class.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeShapeDraft {
    pub shape: String,
    pub class: String,
    /// SHACL instances of the class in the selection.
    pub instances: u64,
    pub closed: bool,
    pub properties: Vec<PropertyDraft>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftSettings {
    pub support: f64,
    pub min_instances: u64,
    pub max_in: usize,
    pub max_count: u64,
    pub closed: bool,
    pub base: String,
    /// The requested classes (empty: every class).
    pub classes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftSelection {
    pub graph: String,
    pub reasoning: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftTotals {
    pub shapes: usize,
    pub property_shapes: usize,
    pub constraints: usize,
    pub rejected: usize,
    /// Classes with instances that got no shape (built-in ones, or below
    /// `min_instances`).
    pub skipped_classes: usize,
}

/// The draft: the shapes with their counts, and the SHACL Turtle, ShExC and shape map
/// texts.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShapesDraft {
    pub draft_format: u32,
    pub dataset: String,
    pub snapshot: SnapshotInfo,
    pub selection: DraftSelection,
    pub options: DraftSettings,
    pub totals: DraftTotals,
    pub shapes: Vec<NodeShapeDraft>,
    /// The shapes graph in Turtle.
    pub shacl: String,
    /// The shapes graph in the SHACL Compact Syntax (SHACLC).
    pub shaclc: String,
    /// The ShEx schema in ShExC.
    pub shex: String,
    /// The query shape map for the ShEx schema: `{FOCUS rdf:type <C>}@<shape>, …`.
    pub shape_map: String,
}

// ------------------------------------------------------------------- profiles ------

const K_IRI: u8 = 1;
const K_BLANK: u8 = 2;
const K_LIT: u8 = 4;
const K_TRIPLE: u8 = 8;

/// The six SHACL node kinds with the kind bits they allow, most specific first.
const NODE_KINDS: [(&str, u8); 6] = [
    ("IRI", K_IRI),
    ("BlankNode", K_BLANK),
    ("Literal", K_LIT),
    ("BlankNodeOrIRI", K_IRI | K_BLANK),
    ("IRIOrLiteral", K_IRI | K_LIT),
    ("BlankNodeOrLiteral", K_BLANK | K_LIT),
];

/// What a stored object is.
#[derive(Clone, Copy, Debug)]
struct ObjInfo {
    kind: u8,
    /// interned datatype (literals)
    dt: u32,
    /// interned language tag + 1 (0: none)
    lang: u32,
    /// a well-formed literal
    valid: bool,
}

/// Datatypes and language tags, interned.
#[derive(Default)]
struct Names {
    names: Vec<String>,
    index: FxHashMap<String, u32>,
}

impl Names {
    fn get(&mut self, s: &str) -> u32 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.names.len() as u32;
        self.names.push(s.to_string());
        self.index.insert(s.to_string(), i);
        i
    }
}

/// Distinct values (or language tags) per instance, as sets, for `sh:in` and
/// `sh:languageIn`.
#[derive(Default)]
struct Sets<T: std::hash::Hash + Eq + Ord + Copy> {
    /// sorted set of an instance → instances with that set
    sigs: FxHashMap<Vec<T>, u64>,
    distinct: FxHashSet<T>,
    /// instances that can satisfy no such constraint (a blank node value, too many
    /// values, an untagged value for language tags)
    never: u64,
}

impl<T: std::hash::Hash + Eq + Ord + Copy> Sets<T> {
    /// Add an instance; `false` once a cap is passed (the sets are then dropped).
    fn add(&mut self, set: Option<&[T]>) -> bool {
        let Some(set) = set else {
            self.never += 1;
            return true;
        };
        for v in set {
            self.distinct.insert(*v);
        }
        *self.sigs.entry(set.to_vec()).or_default() += 1;
        self.distinct.len() <= TRACKED_VALUES && self.sigs.len() <= TRACKED_SETS
    }

    /// Values by descending number of instances that use them (then by value).
    fn by_use(&self) -> Vec<(T, u64)> {
        let mut uses: FxHashMap<T, u64> = FxHashMap::default();
        for (sig, n) in &self.sigs {
            for v in sig {
                *uses.entry(*v).or_default() += n;
            }
        }
        let mut v: Vec<(T, u64)> = uses.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }

    /// Instances whose set lies within the first `k` values of `order`, for every `k`.
    fn covered(&self, order: &[T]) -> Vec<u64> {
        let rank: FxHashMap<T, usize> = order.iter().enumerate().map(|(i, v)| (*v, i)).collect();
        let mut by_rank = vec![0u64; order.len() + 1];
        for (sig, n) in &self.sigs {
            let need = sig
                .iter()
                .map(|v| rank.get(v).map_or(usize::MAX, |r| r + 1))
                .max()
                .unwrap_or(0);
            if need <= order.len() {
                by_rank[need] += n;
            }
        }
        // prefix sums: covered[k] = instances needing at most k values
        let mut acc = 0;
        by_rank
            .into_iter()
            .map(|n| {
                acc += n;
                acc
            })
            .collect()
    }
}

/// The profile of one predicate over the instances of one class.
struct Acc {
    with: u64,
    /// values per instance → instances
    hist: BTreeMap<u64, u64>,
    /// kind bits of an instance's values → instances
    masks: [u64; 16],
    /// datatype → instances whose values are all well-formed literals of it
    dt: FxHashMap<u32, u64>,
    /// class → instances whose values are all instances of it
    cls: FxHashMap<u32, u64>,
    values: Option<Sets<u64>>,
    langs: Option<Sets<u32>>,
    /// instances with two tagged values
    multi_tagged: bool,
    /// instances with two values in one language
    dup_lang: u64,
}

impl Acc {
    fn new() -> Acc {
        Acc {
            with: 0,
            hist: BTreeMap::new(),
            masks: [0; 16],
            dt: FxHashMap::default(),
            cls: FxHashMap::default(),
            values: Some(Sets::default()),
            langs: Some(Sets::default()),
            multi_tagged: false,
            dup_lang: 0,
        }
    }
}

/// The values of one subject for the current predicate, summarized.
struct Summary {
    n: u64,
    mask: u8,
    dt: Option<u32>,
    /// classes every value belongs to (only when every value is an IRI or blank node)
    classes: Vec<u32>,
    /// the values, sorted (`None`: one of them can be in no `sh:in`)
    values: Option<Vec<u64>>,
    /// the language tags, sorted and distinct (`None`: an untagged value)
    langs: Option<Vec<u32>>,
    tagged: usize,
    dup_lang: bool,
}

impl Acc {
    fn add(&mut self, s: &Summary) {
        self.with += 1;
        *self.hist.entry(s.n).or_default() += 1;
        self.masks[s.mask as usize & 15] += 1;
        if let Some(d) = s.dt {
            *self.dt.entry(d).or_default() += 1;
        }
        for c in &s.classes {
            *self.cls.entry(*c).or_default() += 1;
        }
        if let Some(v) = &mut self.values
            && !v.add(s.values.as_deref())
        {
            self.values = None;
        }
        if let Some(l) = &mut self.langs
            && !l.add(s.langs.as_deref())
        {
            self.langs = None;
        }
        self.multi_tagged |= s.tagged >= 2;
        self.dup_lang += s.dup_lang as u64;
    }
}

/// Classes, their closure under `rdfs:subClassOf`, and the class sets of subjects.
struct Types {
    /// class index → class id
    ids: Vec<u64>,
    index: FxHashMap<u64, u32>,
    /// interned, sorted sets of classes (closed under superclasses)
    sets: Vec<Vec<u32>>,
    /// subject → its set
    subject: FxHashMap<u64, u32>,
    /// SHACL instances per class
    instances: Vec<u64>,
    /// class → its subclasses (and itself), for the ShEx class test
    subclasses: Vec<Vec<u32>>,
}

impl Types {
    fn class(&mut self, id: u64) -> u32 {
        if let Some(&i) = self.index.get(&id) {
            return i;
        }
        let i = self.ids.len() as u32;
        self.ids.push(id);
        self.index.insert(id, i);
        i
    }

    fn set_of(&self, id: u64) -> &[u32] {
        self.subject
            .get(&id)
            .map_or(&[], |&s| self.sets[s as usize].as_slice())
    }
}

fn types(
    snap: &Src,
    filter: &GraphFilter,
    budget: &Budget,
    max_entries: usize,
) -> Result<Types, SchemaError> {
    let mut t = Types {
        ids: Vec::new(),
        index: FxHashMap::default(),
        sets: Vec::new(),
        subject: FxHashMap::default(),
        instances: Vec::new(),
        subclasses: Vec::new(),
    };
    // rdfs:subClassOf edges between IRIs in the selection
    let mut supers: Vec<Vec<u32>> = Vec::new();
    if let Some(sub) = snap.lookup_iri(RDFS_SUBCLASS_OF) {
        let mut prev = None;
        let mut edges = Vec::new();
        in_phase(
            snap.for_each_key(Perm::Pso, &[sub.0], budget, |k| {
                if filter.accepts(k[3]) && prev != Some((k[1], k[2])) {
                    prev = Some((k[1], k[2]));
                    edges.push((k[1], k[2]));
                }
            }),
            || "reading rdfs:subClassOf".into(),
        )?;
        for (s, o) in edges {
            if s != o && is_iri(snap, s) && is_iri(snap, o) {
                let (s, o) = (t.class(s), t.class(o));
                if supers.len() < t.ids.len() {
                    supers.resize(t.ids.len(), Vec::new());
                }
                supers[s as usize].push(o);
            }
        }
    }
    // the direct types of each subject (PSO: one run per subject), closed under
    // superclasses
    let mut closure: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
    let mut set_index: FxHashMap<Vec<u32>, u32> = FxHashMap::default();
    let mut direct_memo: FxHashMap<Vec<u32>, u32> = FxHashMap::default();
    if let Some(ty) = snap.lookup_iri(RDF_TYPE) {
        let mut runs: Vec<(u64, Vec<u64>)> = Vec::new();
        let mut prev = None;
        in_phase(
            snap.for_each_key(Perm::Pso, &[ty.0], budget, |k| {
                if !filter.accepts(k[3]) || prev == Some((k[1], k[2])) {
                    return;
                }
                prev = Some((k[1], k[2]));
                match runs.last_mut() {
                    Some((s, cs)) if *s == k[1] => cs.push(k[2]),
                    _ => runs.push((k[1], vec![k[2]])),
                }
            }),
            || "reading rdf:type".into(),
        )?;
        for (s, cs) in runs {
            let mut direct: Vec<u32> = cs
                .into_iter()
                .filter(|&c| is_iri(snap, c))
                .map(|c| t.class(c))
                .collect();
            if direct.is_empty() {
                continue;
            }
            direct.sort_unstable();
            let set = match direct_memo.get(&direct) {
                Some(&i) => i,
                None => {
                    let mut all = Vec::new();
                    for &c in &direct {
                        let cl = closure
                            .entry(c)
                            .or_insert_with(|| close(c, &supers))
                            .clone();
                        all.extend(cl);
                    }
                    all.sort_unstable();
                    all.dedup();
                    let i = match set_index.get(&all) {
                        Some(&i) => i,
                        None => {
                            let i = t.sets.len() as u32;
                            t.sets.push(all.clone());
                            set_index.insert(all, i);
                            i
                        }
                    };
                    direct_memo.insert(direct, i);
                    i
                }
            };
            t.subject.insert(s, set);
        }
    }
    if t.ids.len() > max_entries {
        return Err(SchemaError::TooManyEntries {
            kind: "classes",
            count: t.ids.len(),
            limit: max_entries,
        });
    }
    t.instances = vec![0; t.ids.len()];
    let mut per_set = vec![0u64; t.sets.len()];
    for s in t.subject.values() {
        per_set[*s as usize] += 1;
    }
    for (set, n) in t.sets.iter().zip(per_set) {
        for c in set {
            t.instances[*c as usize] += n;
        }
    }
    // subclasses (reflexive) of every class, from the closures of all classes
    supers.resize(t.ids.len(), Vec::new());
    t.subclasses = vec![Vec::new(); t.ids.len()];
    for c in 0..t.ids.len() as u32 {
        for s in close(c, &supers) {
            t.subclasses[s as usize].push(c);
        }
    }
    Ok(t)
}

/// `c` and every class it reaches over `supers`, sorted.
fn close(c: u32, supers: &[Vec<u32>]) -> Vec<u32> {
    let mut seen = FxHashSet::default();
    let mut stack = vec![c];
    seen.insert(c);
    while let Some(x) = stack.pop() {
        for &y in supers.get(x as usize).map_or(&[][..], |v| v.as_slice()) {
            if seen.insert(y) {
                stack.push(y);
            }
        }
    }
    let mut v: Vec<u32> = seen.into_iter().collect();
    v.sort_unstable();
    v
}

const INLINE_DT: [(Tag, &str); 6] = [
    (Tag::Int, "http://www.w3.org/2001/XMLSchema#integer"),
    (Tag::Decimal, "http://www.w3.org/2001/XMLSchema#decimal"),
    (Tag::Double, "http://www.w3.org/2001/XMLSchema#double"),
    (Tag::Bool, "http://www.w3.org/2001/XMLSchema#boolean"),
    (Tag::DateTime, "http://www.w3.org/2001/XMLSchema#dateTime"),
    (Tag::Date, "http://www.w3.org/2001/XMLSchema#date"),
];

/// The kind, datatype, language and well-formedness of a literal's key.
fn literal_info(key: &[u8], dts: &mut Names, langs: &mut Names) -> ObjInfo {
    let suffix = literal_suffix(key);
    let (dt, lang) = match suffix.first() {
        None => (dts.get("http://www.w3.org/2001/XMLSchema#string"), 0),
        Some(b'@') => {
            let tag = String::from_utf8_lossy(&suffix[1..]);
            let (tag, dt) = match tag.rsplit_once("--") {
                Some((l, "ltr" | "rtl")) => (
                    l.to_string(),
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#dirLangString",
                ),
                _ => (tag.into_owned(), RDF_LANG_STRING),
            };
            (dts.get(dt), langs.get(&tag) + 1)
        }
        Some(_) => (dts.get(&String::from_utf8_lossy(&suffix[1..])), 0),
    };
    let valid = match crate::id::key_to_term(key) {
        Term::Literal(l) => crate::xsd::is_valid(&l),
        _ => false,
    };
    ObjInfo {
        kind: K_LIT,
        dt,
        lang,
        valid,
    }
}

/// Classify the objects of `p` that need their vocabulary key (base-vocabulary literals
/// and delta terms), reading base keys in sorted batches.
fn object_infos(
    snap: &Src,
    p: u64,
    filter: &GraphFilter,
    budget: &Budget,
    dts: &mut Names,
    langs: &mut Names,
) -> crate::Result<FxHashMap<u64, ObjInfo>> {
    let mut base: Vec<u64> = Vec::new();
    let mut delta: Vec<u64> = Vec::new();
    let mut prev = None;
    let v = &snap.generation.vocab;
    snap.for_each_key(Perm::Pos, &[p], budget, |k| {
        let o = k[1];
        if !filter.accepts(k[3]) || prev == Some(o) {
            return;
        }
        prev = Some(o);
        let id = Id(o);
        match id.tag() {
            Tag::Vocab if !v.is_iri(id.payload()) && !v.is_triple(id.payload()) => {
                base.push(id.payload())
            }
            Tag::Delta => delta.push(o),
            _ => {}
        }
    })?;
    let mut out = FxHashMap::default();
    base.sort_unstable();
    for chunk in base.chunks(4096) {
        budget.check()?;
        v.get_sorted(chunk, |payload, key| {
            out.insert(
                Id::new(Tag::Vocab, payload).0,
                literal_info(key, dts, langs),
            );
        });
    }
    for o in delta {
        let info = match snap.key(Id(o)) {
            Some(k) => match k.first() {
                Some(b'"') => literal_info(&k, dts, langs),
                Some(b'<') => ObjInfo::of(K_IRI),
                Some(b'_') => ObjInfo::of(K_BLANK),
                Some(b'(') => ObjInfo::of(K_TRIPLE),
                _ => ObjInfo::of(0),
            },
            None => ObjInfo::of(0),
        };
        out.insert(o, info);
    }
    Ok(out)
}

impl ObjInfo {
    fn of(kind: u8) -> ObjInfo {
        ObjInfo {
            kind,
            dt: 0,
            lang: 0,
            valid: false,
        }
    }
}

/// The interned datatypes of inline literal tags.
struct InlineDts([u32; 6]);

impl InlineDts {
    fn new(dts: &mut Names) -> InlineDts {
        InlineDts(INLINE_DT.map(|(_, d)| dts.get(d)))
    }
}

fn info_of(
    snap: &Snapshot,
    o: u64,
    infos: &FxHashMap<u64, ObjInfo>,
    inline: &InlineDts,
) -> ObjInfo {
    let id = Id(o);
    let tag = id.tag();
    if let Some(i) = INLINE_DT.iter().position(|(t, _)| *t == tag) {
        return ObjInfo {
            kind: K_LIT,
            dt: inline.0[i],
            lang: 0,
            valid: true,
        };
    }
    match tag {
        Tag::BNode => ObjInfo::of(K_BLANK),
        Tag::Vocab => {
            let v = &snap.generation.vocab;
            if v.is_iri(id.payload()) {
                ObjInfo::of(K_IRI)
            } else if v.is_triple(id.payload()) {
                ObjInfo::of(K_TRIPLE)
            } else {
                infos.get(&o).copied().unwrap_or(ObjInfo::of(0))
            }
        }
        _ => infos.get(&o).copied().unwrap_or(ObjInfo::of(0)),
    }
}

fn summarize(
    objs: &[u64],
    snap: &Snapshot,
    types: &Types,
    infos: &FxHashMap<u64, ObjInfo>,
    inline: &InlineDts,
) -> Summary {
    let mut mask = 0u8;
    let mut dt: Option<Option<u32>> = None; // None: unseen; Some(None): mixed
    let mut classes: Option<Vec<u32>> = None;
    let mut eligible = objs.len() <= TRACKED_VALUES;
    let mut langs: Vec<u32> = Vec::new();
    let mut untagged = false;
    for &o in objs {
        let info = info_of(snap, o, infos, inline);
        mask |= info.kind;
        let this_dt = (info.kind == K_LIT && info.valid).then_some(info.dt);
        dt = Some(match dt {
            None => this_dt,
            Some(prev) if prev == this_dt => prev,
            Some(_) => None,
        });
        if info.kind & (K_IRI | K_BLANK) != 0 {
            let cs = types.set_of(o);
            classes = Some(match classes {
                None => cs.to_vec(),
                Some(prev) => prev.into_iter().filter(|c| cs.contains(c)).collect(),
            });
        }
        if info.kind & (K_BLANK | K_TRIPLE) != 0 || info.kind == 0 {
            eligible = false;
        }
        if info.lang > 0 {
            langs.push(info.lang - 1);
        } else {
            untagged = true;
        }
    }
    let tagged = langs.len();
    langs.sort_unstable();
    let before = langs.len();
    langs.dedup();
    let dup_lang = langs.len() < before;
    Summary {
        n: objs.len() as u64,
        mask,
        dt: dt.flatten(),
        classes: if mask & !(K_IRI | K_BLANK) == 0 {
            classes.unwrap_or_default()
        } else {
            Vec::new()
        },
        values: eligible.then(|| objs.to_vec()),
        langs: (!untagged).then_some(langs),
        tagged,
        dup_lang,
    }
}

// ------------------------------------------------------------------ decisions ------

/// `n` of `of` reaches the support.
fn passes(n: u64, of: u64, support: f64) -> bool {
    of > 0 && n as f64 >= support * of as f64 - 1e-9
}

fn constraint(
    component: &'static str,
    value: ConstraintValue,
    applicable: u64,
    satisfied: u64,
) -> ConstraintDraft {
    ConstraintDraft {
        component,
        value,
        applicable,
        satisfied,
        excluded: applicable.saturating_sub(satisfied),
    }
}

/// Everything a decision needs besides the accumulator.
struct Ctx<'a> {
    snap: &'a Snapshot,
    types: &'a Types,
    iris: &'a [Option<String>],
    dts: &'a Names,
    langs: &'a Names,
    opts: &'a DraftOptions,
}

impl Ctx<'_> {
    fn term(&self, id: u64) -> Option<String> {
        self.snap.term(Id(id)).map(|t| t.to_string())
    }
}

fn decide(path: String, instances: u64, acc: &Acc, cx: &Ctx) -> PropertyDraft {
    let s = cx.opts.support;
    let w = acc.with;
    let mut out = PropertyDraft {
        path,
        instances: w,
        max_values: acc.hist.keys().next_back().copied().unwrap_or(0),
        constraints: Vec::new(),
        rejected: Vec::new(),
    };
    let mut push = |ok: bool, c: ConstraintDraft| {
        if ok {
            out.constraints.push(c);
        } else {
            out.rejected.push(c);
        }
    };
    // sh:minCount: the largest k that enough instances reach
    let at_least = |k: u64| acc.hist.range(k..).map(|(_, n)| n).sum::<u64>();
    let one = at_least(1);
    if passes(one, instances, s) {
        let mut k = 1;
        for &n in acc.hist.keys() {
            if n > k && passes(at_least(n), instances, s) {
                k = n;
            }
        }
        push(
            true,
            constraint(
                "minCount",
                ConstraintValue::Count(k),
                instances,
                at_least(k),
            ),
        );
    } else {
        push(
            false,
            constraint("minCount", ConstraintValue::Count(1), instances, one),
        );
    }
    // sh:maxCount: the smallest k within the limit that enough instances keep to
    if cx.opts.max_count > 0 && w > 0 {
        let at_most = |k: u64| acc.hist.range(..=k).map(|(_, n)| n).sum::<u64>();
        let limit = cx.opts.max_count;
        if passes(at_most(limit), w, s) {
            let k = (1..=limit)
                .find(|&k| passes(at_most(k), w, s))
                .unwrap_or(limit);
            push(
                true,
                constraint("maxCount", ConstraintValue::Count(k), w, at_most(k)),
            );
        } else {
            push(
                false,
                constraint("maxCount", ConstraintValue::Count(limit), w, at_most(limit)),
            );
        }
    }
    // sh:datatype
    let datatype = acc
        .dt
        .iter()
        .max_by(|a, b| {
            a.1.cmp(b.1)
                .then_with(|| cx.dts.names[*b.0 as usize].cmp(&cx.dts.names[*a.0 as usize]))
        })
        .map(|(d, n)| (cx.dts.names[*d as usize].clone(), *n));
    let mut drafted_dt = None;
    if let Some((d, n)) = &datatype {
        let ok = passes(*n, w, s);
        if ok {
            drafted_dt = Some(d.clone());
        }
        push(
            ok,
            constraint("datatype", ConstraintValue::Iri(d.clone()), w, *n),
        );
    }
    // sh:nodeKind: the most specific kind that passes, else the one closest to passing
    let covered = |bits: u8| -> u64 {
        (1..16u8)
            .filter(|m| m & !bits == 0)
            .map(|m| acc.masks[m as usize])
            .sum()
    };
    let kinds: Vec<(&str, u64)> = NODE_KINDS.iter().map(|(n, b)| (*n, covered(*b))).collect();
    match kinds.iter().find(|(_, n)| passes(*n, w, s)) {
        Some((k, n)) => {
            if !(*k == "Literal" && drafted_dt.is_some()) {
                push(
                    true,
                    constraint("nodeKind", ConstraintValue::Iri(format!("{SH}{k}")), w, *n),
                );
            }
        }
        None => {
            if let Some((k, n)) = kinds.iter().filter(|(_, n)| *n > 0).max_by_key(|(_, n)| *n) {
                push(
                    false,
                    constraint("nodeKind", ConstraintValue::Iri(format!("{SH}{k}")), w, *n),
                );
            }
        }
    }
    // sh:class: the class outside the built-in namespaces with the most instances whose
    // values all belong to it; ties go to the smaller class
    let class = acc
        .cls
        .iter()
        .filter_map(|(c, n)| Some((cx.iris[*c as usize].as_deref()?, *c, *n)))
        .filter(|(iri, _, _)| !builtin(iri))
        .max_by(|a, b| {
            a.2.cmp(&b.2)
                .then_with(|| {
                    cx.types.instances[b.1 as usize].cmp(&cx.types.instances[a.1 as usize])
                })
                .then_with(|| b.0.cmp(a.0))
        });
    if let Some((iri, _, n)) = class {
        push(
            passes(n, w, s),
            constraint("class", ConstraintValue::Iri(iri.to_string()), w, n),
        );
    }
    // sh:in: the most used values, each used by two instances or more, up to max_in
    if cx.opts.max_in > 0
        && drafted_dt.as_deref() != Some(XSD_BOOLEAN)
        && let Some(sets) = &acc.values
    {
        let order: Vec<u64> = sets
            .by_use()
            .into_iter()
            .take_while(|(_, n)| *n >= 2)
            .take(cx.opts.max_in)
            .map(|(v, _)| v)
            .collect();
        if !order.is_empty() {
            let cov = sets.covered(&order);
            let k = (1..=order.len()).find(|&k| passes(cov[k], w, s));
            let (ok, k) = match k {
                Some(k) => (true, k),
                None => (false, order.len()),
            };
            let terms: Option<Vec<String>> = order[..k].iter().map(|v| cx.term(*v)).collect();
            if let Some(terms) = terms {
                push(
                    ok,
                    constraint("in", ConstraintValue::List(terms), w, cov[k]),
                );
            }
        }
    }
    // sh:languageIn and sh:uniqueLang
    if let Some(sets) = &acc.langs
        && !sets.distinct.is_empty()
    {
        let order: Vec<u32> = sets.by_use().into_iter().map(|(v, _)| v).collect();
        let cov = sets.covered(&order);
        let k = (1..=order.len()).find(|&k| passes(cov[k], w, s));
        let (ok, k) = match k {
            Some(k) => (true, k),
            None => (false, order.len()),
        };
        let tags = order[..k]
            .iter()
            .map(|l| cx.langs.names[*l as usize].clone())
            .collect();
        push(
            ok,
            constraint("languageIn", ConstraintValue::List(tags), w, cov[k]),
        );
    }
    if acc.multi_tagged {
        let n = w - acc.dup_lang;
        push(
            passes(n, w, s),
            constraint("uniqueLang", ConstraintValue::Bool(true), w, n),
        );
    }
    out
}

// --------------------------------------------------------------------- driver ------

/// Draft SHACL shapes and a ShEx schema from `snap`.
pub fn draft_shapes(snap: &Arc<Snapshot>, opts: &DraftOptions) -> Result<ShapesDraft, SchemaError> {
    let so = &opts.schema;
    let budget = Budget {
        deadline: so.deadline,
        cancel: so.cancel.as_deref(),
    };
    in_phase(budget.check(), || "starting".into())?;
    let inferred = so
        .inferred_graph
        .as_deref()
        .and_then(|g| snap.lookup_iri(g))
        .map(|g| g.0);
    let mut filter = resolve(snap, &so.graph, inferred, so.include_inferred)?;
    if let Some(a) = so.graphs.as_ref().filter(|a| !a.reads_all()) {
        filter = within_view(snap, filter, &so.graph, a)?;
    }
    let src = in_phase(Src::for_filters(snap, &[&filter], &budget), || {
        "reading the selected graphs".into()
    })?;
    let snap = &src;
    let types = types(snap, &filter, &budget, so.max_entries)?;
    let mut iris = Iris {
        snap,
        cache: FxHashMap::default(),
    };
    let class_iris: Vec<Option<String>> = types.ids.iter().map(|&c| iris.get(c)).collect();

    // the classes that get a shape
    let requested: FxHashSet<&str> = opts.classes.iter().map(String::as_str).collect();
    let mut drafted = vec![false; types.ids.len()];
    let mut skipped = 0;
    for (c, iri) in class_iris.iter().enumerate() {
        let Some(iri) = iri else { continue };
        let n = types.instances[c];
        let wanted = if requested.is_empty() {
            !builtin(iri)
        } else {
            requested.contains(iri.as_str())
        };
        if wanted && n >= opts.min_instances.max(1) {
            drafted[c] = true;
        } else if n > 0 && requested.is_empty() {
            skipped += 1;
        }
    }

    // per predicate, the profile of every drafted class
    let rdf_type = snap.lookup_iri(RDF_TYPE).map(|i| i.0);
    let preds = in_phase(snap.distinct_first(Perm::Pso), || {
        "listing predicates".into()
    })?;
    let mut dts = Names::default();
    let mut langs = Names::default();
    let inline = InlineDts::new(&mut dts);
    let mut props: Vec<Vec<PropertyDraft>> = (0..types.ids.len()).map(|_| Vec::new()).collect();
    if drafted.iter().any(|d| *d) {
        for (i, &p) in preds.iter().enumerate() {
            if Some(p) == rdf_type || !is_iri(snap, p) {
                continue;
            }
            let phase = || format!("profiling predicates ({}/{})", i + 1, preds.len());
            let infos = in_phase(
                object_infos(snap, p, &filter, &budget, &mut dts, &mut langs),
                phase,
            )?;
            let mut accs: FxHashMap<u32, Acc> = FxHashMap::default();
            let mut subject: Option<u64> = None;
            let mut objs: Vec<u64> = Vec::new();
            let flush = |s: u64, objs: &[u64], accs: &mut FxHashMap<u32, Acc>| {
                let set = types.set_of(s);
                if !set.iter().any(|c| drafted[*c as usize]) {
                    return;
                }
                let sum = summarize(objs, snap, &types, &infos, &inline);
                for c in set {
                    if drafted[*c as usize] {
                        accs.entry(*c).or_insert_with(Acc::new).add(&sum);
                    }
                }
            };
            let mut prev = None;
            in_phase(
                snap.for_each_key(Perm::Pso, &[p], &budget, |k| {
                    if !filter.accepts(k[3]) || prev == Some((k[1], k[2])) {
                        return;
                    }
                    prev = Some((k[1], k[2]));
                    if subject != Some(k[1]) {
                        if let Some(s) = subject {
                            flush(s, &objs, &mut accs);
                        }
                        subject = Some(k[1]);
                        objs.clear();
                    }
                    objs.push(k[2]);
                }),
                phase,
            )?;
            if let Some(s) = subject {
                flush(s, &objs, &mut accs);
            }
            if accs.is_empty() {
                continue;
            }
            let Some(path) = iris.get(p) else { continue };
            let cx = Ctx {
                snap,
                types: &types,
                iris: &class_iris,
                dts: &dts,
                langs: &langs,
                opts,
            };
            for (c, acc) in accs {
                let d = decide(path.clone(), types.instances[c as usize], &acc, &cx);
                props[c as usize].push(d);
            }
        }
    }

    // shapes in class IRI order
    let mut order: Vec<usize> = (0..types.ids.len())
        .filter(|&c| drafted[c] && class_iris[c].is_some())
        .collect();
    order.sort_by(|a, b| class_iris[*a].cmp(&class_iris[*b]));
    let mut names = ShapeNames::default();
    let mut shapes = Vec::new();
    for c in order {
        let class = class_iris[c].clone().unwrap_or_default();
        let instances = types.instances[c];
        let mut properties = std::mem::take(&mut props[c]);
        // most used first, then by IRI
        properties.sort_by(|a, b| {
            b.instances
                .cmp(&a.instances)
                .then_with(|| a.path.cmp(&b.path))
        });
        shapes.push(NodeShapeDraft {
            shape: names.name(&opts.base, &class),
            class,
            instances,
            closed: opts.closed,
            properties,
        });
    }
    // requested classes the selection has no instances of still get an empty shape
    for iri in &opts.classes {
        if !shapes.iter().any(|s| &s.class == iri) {
            shapes.push(NodeShapeDraft {
                shape: names.name(&opts.base, iri),
                class: iri.clone(),
                instances: 0,
                closed: opts.closed,
                properties: Vec::new(),
            });
        }
    }

    let mut totals = DraftTotals {
        shapes: shapes.len(),
        skipped_classes: skipped,
        ..Default::default()
    };
    for s in &shapes {
        totals.property_shapes += s.properties.len();
        for p in &s.properties {
            totals.constraints += p.constraints.len();
            totals.rejected += p.rejected.len();
        }
    }
    let subclasses = |class: &str| -> Vec<String> {
        let Some(c) = class_iris.iter().position(|i| i.as_deref() == Some(class)) else {
            return vec![class.to_string()];
        };
        let mut v: Vec<String> = types.subclasses[c]
            .iter()
            .filter_map(|s| class_iris[*s as usize].clone())
            .collect();
        v.sort();
        v
    };
    let mut draft = ShapesDraft {
        draft_format: DRAFT_FORMAT,
        dataset: opts.dataset.clone(),
        snapshot: SnapshotInfo {
            version: snapshot_identity(snap),
            commit: snap.commit,
            generation: snap.generation.name.clone(),
            computed_at: crate::builder::now_rfc3339(),
        },
        selection: DraftSelection {
            graph: so.graph.name().to_string(),
            reasoning: so.include_inferred,
        },
        options: DraftSettings {
            support: opts.support,
            min_instances: opts.min_instances,
            max_in: opts.max_in,
            max_count: opts.max_count,
            closed: opts.closed,
            base: opts.base.clone(),
            classes: opts.classes.clone(),
        },
        totals,
        shapes,
        shacl: String::new(),
        shaclc: String::new(),
        shex: String::new(),
        shape_map: String::new(),
    };
    let names = render::Prefixes::new(&opts.prefixes, &opts.base);
    draft.shacl = render::shacl(&draft, &names);
    draft.shaclc = render::shaclc(&draft, &names);
    draft.shex = render::shexc(&draft, &names, &subclasses);
    draft.shape_map = render::shape_map(&draft, &names);
    Ok(draft)
}

/// Shape IRIs from class local names, kept distinct.
#[derive(Default)]
struct ShapeNames {
    used: FxHashSet<String>,
}

impl ShapeNames {
    fn name(&mut self, base: &str, class: &str) -> String {
        let local = class
            .rsplit(['#', '/', ':'])
            .find(|s| !s.is_empty())
            .unwrap_or("");
        let mut l: String = local
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if !l.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            l.insert(0, 'C');
        }
        let stem = format!("{base}{l}Shape");
        let mut name = stem.clone();
        let mut i = 2;
        while !self.used.insert(name.clone()) {
            name = format!("{stem}{i}");
            i += 1;
        }
        name
    }
}

mod render;
