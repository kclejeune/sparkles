//! The entity index of one branch: what it stores, its build from a snapshot and its
//! maintenance from a commit's changes.

use super::text::{name_words, normalize, words};
use crate::error::{Error, Result};
use crate::id::Id;
use crate::index::Perm;
use crate::store::{Chunk, Snapshot};
use oxrdf::Term;
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;
use std::collections::BTreeMap;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const SKOS: &str = "http://www.w3.org/2004/02/skos/core#";
const SKOSXL: &str = "http://www.w3.org/2008/05/skos-xl#";
const OBO_IN_OWL: &str = "http://www.geneontology.org/formats/oboInOwl#";
const IAO_DEFINITION: &str = "http://purl.obolibrary.org/obo/IAO_0000115";
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The most entities an index holds; a larger dataset is refused.
pub const MAX_ENTITIES: usize = 8_000_000;

/// The kinds of entity a search filters on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Class,
    ObjectProperty,
    DataProperty,
    AnnotationProperty,
    /// `rdf:Property` without a more specific OWL type
    Property,
    Individual,
    Datatype,
    Concept,
    ConceptScheme,
    Collection,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::Class,
        Kind::ObjectProperty,
        Kind::DataProperty,
        Kind::AnnotationProperty,
        Kind::Property,
        Kind::Individual,
        Kind::Datatype,
        Kind::Concept,
        Kind::ConceptScheme,
        Kind::Collection,
    ];

    /// The name of the kind on the wire.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Class => "class",
            Kind::ObjectProperty => "objectProperty",
            Kind::DataProperty => "dataProperty",
            Kind::AnnotationProperty => "annotationProperty",
            Kind::Property => "property",
            Kind::Individual => "individual",
            Kind::Datatype => "datatype",
            Kind::Concept => "concept",
            Kind::ConceptScheme => "conceptScheme",
            Kind::Collection => "collection",
        }
    }

    /// The kinds a filter name selects: a kind's own name, or `property` for every kind
    /// of property.
    pub fn parse_filter(s: &str) -> Option<Vec<Kind>> {
        match s {
            "property" => Some(vec![
                Kind::ObjectProperty,
                Kind::DataProperty,
                Kind::AnnotationProperty,
                Kind::Property,
            ]),
            "namedIndividual" => Some(vec![Kind::Individual]),
            _ => Kind::ALL
                .into_iter()
                .find(|k| k.name() == s)
                .map(|k| vec![k]),
        }
    }

    pub(super) fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

/// The types that make an IRI an entity, with the kind each gives. A type fact is kept by
/// its index here, so removing one of two types of the same kind keeps the kind.
pub(super) const TYPES: [(&str, &str, Kind); 13] = [
    (OWL, "Class", Kind::Class),
    (RDFS, "Class", Kind::Class),
    (OWL, "ObjectProperty", Kind::ObjectProperty),
    (OWL, "DatatypeProperty", Kind::DataProperty),
    (OWL, "AnnotationProperty", Kind::AnnotationProperty),
    (RDF, "Property", Kind::Property),
    (OWL, "NamedIndividual", Kind::Individual),
    (OWL, "Datatype", Kind::Datatype),
    (RDFS, "Datatype", Kind::Datatype),
    (SKOS, "Concept", Kind::Concept),
    (SKOS, "ConceptScheme", Kind::ConceptScheme),
    (SKOS, "Collection", Kind::Collection),
    (SKOS, "OrderedCollection", Kind::Collection),
];

/// The part of an entity's description a value comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Field {
    Label,
    Synonym,
    ShortForm,
    Definition,
}

impl Field {
    pub fn name(self) -> &'static str {
        match self {
            Field::Label => "label",
            Field::Synonym => "synonym",
            Field::ShortForm => "shortForm",
            Field::Definition => "definition",
        }
    }
}

/// A property whose values are indexed.
pub(super) struct Prop {
    pub ns: &'static str,
    pub local: &'static str,
    pub field: Field,
    /// the order of values of one field: exact synonyms before related ones
    pub rank: u8,
    /// a SKOS-XL label property, whose object is a label node
    pub xl: bool,
}

/// The index in [`PROPS`] of `oboInOwl:id`.
pub(super) const OBO_ID: u8 = 12;
/// The index in [`PROPS`] of `skos:notation`.
pub(super) const NOTATION: u8 = 13;
/// The local name of an entity's IRI, a value no triple states.
pub(super) const LOCAL_NAME: u8 = 16;

pub(super) const PROPS: [Prop; 17] = {
    const fn p(ns: &'static str, local: &'static str, field: Field, rank: u8, xl: bool) -> Prop {
        Prop {
            ns,
            local,
            field,
            rank,
            xl,
        }
    }
    use Field::*;
    [
        p(SKOS, "prefLabel", Label, 0, false),
        p(SKOSXL, "prefLabel", Label, 0, true),
        p(RDFS, "label", Label, 0, false),
        p(SKOS, "altLabel", Synonym, 0, false),
        p(SKOSXL, "altLabel", Synonym, 0, true),
        p(OBO_IN_OWL, "hasExactSynonym", Synonym, 0, false),
        p(OBO_IN_OWL, "hasSynonym", Synonym, 1, false),
        p(OBO_IN_OWL, "hasRelatedSynonym", Synonym, 1, false),
        p(OBO_IN_OWL, "hasBroadSynonym", Synonym, 1, false),
        p(OBO_IN_OWL, "hasNarrowSynonym", Synonym, 1, false),
        p(SKOS, "hiddenLabel", Synonym, 2, false),
        p(SKOSXL, "hiddenLabel", Synonym, 2, true),
        p(OBO_IN_OWL, "id", ShortForm, 0, false),
        p(SKOS, "notation", ShortForm, 0, false),
        p(SKOS, "definition", Definition, 0, false),
        p(IAO_DEFINITION, "", Definition, 0, false),
        // the local name, which no triple states
        p("", "", ShortForm, 1, false),
    ]
};

impl Prop {
    pub(super) fn iri(&self) -> String {
        format!("{}{}", self.ns, self.local)
    }
}

/// A value's graph when it holds in every graph its entity is declared in (the local name).
pub(super) const ANY_GRAPH: u32 = u32::MAX;
/// A value that comes from no SKOS-XL label node.
pub(super) const NO_NODE: u32 = u32::MAX;

/// One indexed value of an entity.
pub(super) struct Value {
    pub entity: u32,
    pub graph: u32,
    /// the SKOS-XL label node it comes from, or [`NO_NODE`]
    pub node: u32,
    /// index of the language tag in [`Index::langs`] (0: no tag)
    pub lang: u16,
    /// index of the datatype in [`Index::datatypes`] (0: `xsd:string` or a language tag)
    pub datatype: u16,
    pub prop: u8,
    /// the text; empty once the value is removed
    pub text: Box<str>,
}

impl Value {
    pub(super) fn field(&self) -> Field {
        PROPS[self.prop as usize].field
    }
    pub(super) fn live(&self) -> bool {
        !self.text.is_empty()
    }
}

/// One entity.
#[derive(Default)]
pub(super) struct Entity {
    /// empty once the entity is removed
    pub iri: Box<str>,
    /// the type facts: graph and index in [`TYPES`]
    pub types: SmallVec<[(u32, u8); 1]>,
    /// the graphs of the `owl:deprecated true` facts, with the literal of each
    pub deprecated: Vec<(u32, Box<str>)>,
    pub values: SmallVec<[u32; 4]>,
    /// the SKOS-XL label links: property, graph and node
    pub links: Vec<(u8, u32, u32)>,
}

/// A SKOS-XL label node: its literal forms and the entities that link to it.
#[derive(Default)]
pub(super) struct Node {
    /// the node's term in N-Triples, empty once removed
    pub key: Box<str>,
    /// text, language, datatype and graph of each `skosxl:literalForm`
    pub forms: Vec<(Box<str>, u16, u16, u32)>,
    /// entity, property and graph of each link
    pub links: Vec<(u32, u8, u32)>,
}

/// Normalized whole texts and the values that have them.
pub(super) type Texts = BTreeMap<Box<[u8]>, SmallVec<[u32; 2]>>;

/// The entity index of one state of a branch.
#[derive(Default)]
pub(super) struct Index {
    /// the commit it reflects
    pub seq: u64,
    /// graph names (`""` for the default graph)
    pub graphs: Vec<Box<str>>,
    pub graph_ids: FxHashMap<Box<str>, u32>,
    /// language tags in lower case (index 0: none)
    pub langs: Vec<Box<str>>,
    lang_ids: FxHashMap<Box<str>, u16>,
    /// datatype IRIs (index 0: `xsd:string` or a language tag)
    pub datatypes: Vec<Box<str>>,
    datatype_ids: FxHashMap<Box<str>, u16>,
    pub entities: Vec<Entity>,
    pub by_iri: BTreeMap<Box<str>, u32>,
    free_entities: Vec<u32>,
    pub values: Vec<Value>,
    pub dead_values: usize,
    /// the normalized whole text of label values
    pub labels: Texts,
    /// the normalized whole text of synonym and short-form values
    pub alts: Texts,
    /// the words of every value, each with the sorted ids of the values that have it
    pub words: BTreeMap<Box<[u8]>, Vec<u32>>,
    pub nodes: Vec<Node>,
    node_ids: FxHashMap<Box<str>, u32>,
    free_nodes: Vec<u32>,
    pub live_entities: usize,
}

/// What a watched predicate is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Watch {
    Type,
    Prop(u8),
    Deprecated,
    LiteralForm,
}

/// The ids of the watched IRIs in one snapshot.
struct Vocab {
    preds: FxHashMap<Id, Watch>,
    types: FxHashMap<Id, u8>,
}

impl Vocab {
    fn of(snap: &Snapshot) -> Vocab {
        let mut preds = FxHashMap::default();
        let mut types = FxHashMap::default();
        if let Some(id) = snap.lookup_iri(&format!("{RDF}type")) {
            preds.insert(id, Watch::Type);
        }
        for (i, p) in PROPS.iter().enumerate() {
            if p.ns.is_empty() {
                continue;
            }
            if let Some(id) = snap.lookup_iri(&p.iri()) {
                preds.insert(id, Watch::Prop(i as u8));
            }
        }
        if let Some(id) = snap.lookup_iri(&format!("{OWL}deprecated")) {
            preds.insert(id, Watch::Deprecated);
        }
        if let Some(id) = snap.lookup_iri(&format!("{SKOSXL}literalForm")) {
            preds.insert(id, Watch::LiteralForm);
        }
        for (i, (ns, local, _)) in TYPES.iter().enumerate() {
            if let Some(id) = snap.lookup_iri(&format!("{ns}{local}")) {
                types.insert(id, i as u8);
            }
        }
        Vocab { preds, types }
    }
}

/// A literal's parts as the index keeps them.
struct Lit {
    text: String,
    lang: String,
    datatype: String,
}

fn literal(snap: &Snapshot, id: Id) -> Option<Lit> {
    match snap.term(id)? {
        Term::Literal(l) => {
            let lang = l
                .language()
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            let datatype = if lang.is_empty() && l.datatype().as_str() != XSD_STRING {
                l.datatype().as_str().to_string()
            } else {
                String::new()
            };
            Some(Lit {
                text: l.value().to_string(),
                lang,
                datatype,
            })
        }
        _ => None,
    }
}

/// Whether a literal says `true`, as `owl:deprecated` takes it.
fn truthy(snap: &Snapshot, id: Id) -> Option<String> {
    match snap.term(id)? {
        Term::Literal(l) if matches!(l.value(), "true" | "1") => Some(l.to_string()),
        _ => None,
    }
}

fn iri_of(snap: &Snapshot, id: Id) -> Option<String> {
    match snap.term(id)? {
        Term::NamedNode(n) => Some(n.into_string()),
        _ => None,
    }
}

/// The key of a label node: its term in N-Triples, or `None` for a literal.
fn node_key(snap: &Snapshot, id: Id) -> Option<String> {
    match snap.term(id)? {
        t @ (Term::NamedNode(_) | Term::BlankNode(_)) => Some(t.to_string()),
        _ => None,
    }
}

/// The local name of an IRI: what follows its last `#`, `/` or `:`.
pub(super) fn local_name(iri: &str) -> &str {
    let cut = iri
        .rfind(['#', '/'])
        .or_else(|| iri.rfind(':'))
        .map_or(0, |i| i + 1);
    &iri[cut..]
}

/// Insert `v` into a sorted id list.
fn insert_sorted(list: &mut Vec<u32>, v: u32) {
    match list.last() {
        Some(&l) if l < v => list.push(v),
        None => list.push(v),
        _ => {
            if let Err(i) = list.binary_search(&v) {
                list.insert(i, v);
            }
        }
    }
}

fn remove_sorted(list: &mut Vec<u32>, v: u32) {
    if let Ok(i) = list.binary_search(&v) {
        list.remove(i);
    }
}

/// The graph id of a stored graph, memoized per build or update.
struct Graphs<'a> {
    snap: &'a Snapshot,
    memo: FxHashMap<Id, u32>,
}

impl Index {
    fn graph(&mut self, g: &mut Graphs<'_>, id: Id) -> Option<u32> {
        if let Some(&n) = g.memo.get(&id) {
            return Some(n);
        }
        let name = if id == Id::DEFAULT_GRAPH {
            String::new()
        } else {
            iri_of(g.snap, id)?
        };
        let n = match self.graph_ids.get(name.as_str()) {
            Some(&n) => n,
            None => {
                let n = self.graphs.len() as u32;
                self.graphs.push(name.clone().into());
                self.graph_ids.insert(name.into(), n);
                n
            }
        };
        g.memo.insert(id, n);
        Some(n)
    }

    fn lang(&mut self, tag: &str) -> u16 {
        if tag.is_empty() {
            return 0;
        }
        if let Some(&n) = self.lang_ids.get(tag) {
            return n;
        }
        let n = self.langs.len().min(u16::MAX as usize) as u16;
        self.langs.push(tag.into());
        self.lang_ids.insert(tag.into(), n);
        n
    }

    fn datatype(&mut self, dt: &str) -> u16 {
        if dt.is_empty() {
            return 0;
        }
        if let Some(&n) = self.datatype_ids.get(dt) {
            return n;
        }
        let n = self.datatypes.len().min(u16::MAX as usize) as u16;
        self.datatypes.push(dt.into());
        self.datatype_ids.insert(dt.into(), n);
        n
    }

    fn empty() -> Index {
        Index {
            langs: vec!["".into()],
            datatypes: vec!["".into()],
            ..Default::default()
        }
    }

    /// The entity of `iri`, created when missing; `true` when it was created.
    fn entity(&mut self, iri: &str) -> Result<(u32, bool)> {
        if let Some(&e) = self.by_iri.get(iri) {
            return Ok((e, false));
        }
        if self.live_entities >= MAX_ENTITIES {
            return Err(Error::invalid(format!(
                "the entity search indexes at most {MAX_ENTITIES} entities"
            )));
        }
        let ent = Entity {
            iri: iri.into(),
            ..Default::default()
        };
        let e = match self.free_entities.pop() {
            Some(e) => {
                self.entities[e as usize] = ent;
                e
            }
            None => {
                self.entities.push(ent);
                (self.entities.len() - 1) as u32
            }
        };
        self.by_iri.insert(iri.into(), e);
        self.live_entities += 1;
        let local = local_name(iri).to_string();
        if !local.is_empty() {
            self.add_value(e, LOCAL_NAME, ANY_GRAPH, NO_NODE, &local, 0, 0);
        }
        Ok((e, true))
    }

    /// Remove an entity and everything it holds.
    fn drop_entity(&mut self, e: u32) {
        let ent = std::mem::take(&mut self.entities[e as usize]);
        if ent.iri.is_empty() {
            return;
        }
        for v in ent.values {
            self.kill_value(v);
        }
        // an entity can link one node through several properties or graphs
        let mut nodes: Vec<u32> = ent.links.iter().map(|l| l.2).collect();
        nodes.sort_unstable();
        nodes.dedup();
        for n in nodes {
            let node = &mut self.nodes[n as usize];
            node.links.retain(|(le, _, _)| *le != e);
            self.release_node(n);
        }
        self.by_iri.remove(&*ent.iri);
        self.free_entities.push(e);
        self.live_entities -= 1;
    }

    #[allow(clippy::too_many_arguments)]
    fn add_value(
        &mut self,
        e: u32,
        prop: u8,
        graph: u32,
        node: u32,
        text: &str,
        lang: u16,
        datatype: u16,
    ) {
        if text.is_empty() {
            return;
        }
        let id = self.values.len() as u32;
        let field = PROPS[prop as usize].field;
        let n = normalize(text);
        if n.is_empty() {
            // nothing to search: keep the value for its entity's label only
        } else {
            if let Some(m) = self.texts_of(field) {
                m.entry(n.as_bytes().into()).or_default().push(id);
            }
            let mut seen: SmallVec<[&str; 16]> = SmallVec::new();
            for w in words(&n) {
                if !seen.contains(&w) {
                    seen.push(w);
                    insert_sorted(self.words.entry(w.as_bytes().into()).or_default(), id);
                }
            }
            if prop == LOCAL_NAME {
                for w in name_words(text) {
                    insert_sorted(self.words.entry(w.as_bytes().into()).or_default(), id);
                }
            }
        }
        self.values.push(Value {
            entity: e,
            graph,
            node,
            lang,
            datatype,
            prop,
            text: text.into(),
        });
        self.entities[e as usize].values.push(id);
    }

    /// The map of whole texts of a field's values (none for definitions).
    fn texts_of(&mut self, f: Field) -> Option<&mut Texts> {
        match f {
            Field::Label => Some(&mut self.labels),
            Field::Synonym | Field::ShortForm => Some(&mut self.alts),
            Field::Definition => None,
        }
    }

    /// Take value `v` out of the dictionaries and mark it removed (its entity's list is
    /// the caller's).
    fn kill_value(&mut self, v: u32) {
        let val = &mut self.values[v as usize];
        if !val.live() {
            return;
        }
        let text = std::mem::take(&mut val.text);
        let field = val.field();
        let prop = val.prop;
        let n = normalize(&text);
        if !n.is_empty() {
            if let Some(m) = self.texts_of(field)
                && let Some(list) = m.get_mut(n.as_bytes())
            {
                list.retain(|x| *x != v);
                if list.is_empty() {
                    m.remove(n.as_bytes());
                }
            }
            let mut ws: Vec<String> = words(&n).map(str::to_string).collect();
            if prop == LOCAL_NAME {
                ws.extend(name_words(&text));
            }
            for w in ws {
                if let Some(list) = self.words.get_mut(w.as_bytes()) {
                    remove_sorted(list, v);
                    if list.is_empty() {
                        self.words.remove(w.as_bytes());
                    }
                }
            }
        }
        self.dead_values += 1;
    }

    /// Remove one value of entity `e` with these parts.
    #[allow(clippy::too_many_arguments)]
    fn remove_value(
        &mut self,
        e: u32,
        prop: u8,
        graph: u32,
        node: u32,
        text: &str,
        lang: u16,
        datatype: u16,
    ) {
        let ent = &self.entities[e as usize];
        let found = ent.values.iter().position(|&v| {
            let x = &self.values[v as usize];
            x.prop == prop
                && x.graph == graph
                && x.node == node
                && x.lang == lang
                && x.datatype == datatype
                && &*x.text == text
        });
        if let Some(i) = found {
            let v = self.entities[e as usize].values.remove(i);
            self.kill_value(v);
        }
    }

    fn node(&mut self, key: &str) -> (u32, bool) {
        if let Some(&n) = self.node_ids.get(key) {
            return (n, false);
        }
        let node = Node {
            key: key.into(),
            ..Default::default()
        };
        let n = match self.free_nodes.pop() {
            Some(n) => {
                self.nodes[n as usize] = node;
                n
            }
            None => {
                self.nodes.push(node);
                (self.nodes.len() - 1) as u32
            }
        };
        self.node_ids.insert(key.into(), n);
        (n, true)
    }

    fn release_node(&mut self, n: u32) {
        // a released slot has no key, and goes on the free list once
        if !self.nodes[n as usize].links.is_empty() || self.nodes[n as usize].key.is_empty() {
            return;
        }
        let node = std::mem::take(&mut self.nodes[n as usize]);
        self.node_ids.remove(&*node.key);
        self.free_nodes.push(n);
    }

    /// Link entity `e` to label node `n` through `prop` in `graph`, with a value for
    /// each of the node's forms.
    fn link(&mut self, e: u32, prop: u8, graph: u32, n: u32) {
        self.entities[e as usize].links.push((prop, graph, n));
        self.nodes[n as usize].links.push((e, prop, graph));
        let forms = self.nodes[n as usize].forms.clone();
        for (text, lang, dt, _) in forms {
            self.add_value(e, prop, graph, n, &text, lang, dt);
        }
    }

    fn unlink(&mut self, e: u32, prop: u8, graph: u32, n: u32) {
        let ent = &mut self.entities[e as usize];
        let Some(i) = ent.links.iter().position(|l| *l == (prop, graph, n)) else {
            return;
        };
        ent.links.remove(i);
        let values = &self.values;
        let (kill, keep): (SmallVec<[u32; 4]>, SmallVec<[u32; 4]>) =
            ent.values.iter().copied().partition(|&v| {
                let x = &values[v as usize];
                x.node == n && x.prop == prop && x.graph == graph
            });
        ent.values = keep;
        for v in kill {
            self.kill_value(v);
        }
        let node = &mut self.nodes[n as usize];
        if let Some(i) = node.links.iter().position(|l| *l == (e, prop, graph)) {
            node.links.remove(i);
        }
        self.release_node(n);
    }

    /// Read the literal forms of node `n` from the snapshot.
    fn load_forms(&mut self, snap: &Snapshot, g: &mut Graphs<'_>, voc: &Vocab, id: Id, n: u32) {
        let Some(lf) = voc
            .preds
            .iter()
            .find(|(_, w)| **w == Watch::LiteralForm)
            .map(|(id, _)| *id)
        else {
            return;
        };
        let mut quads: Vec<[Id; 4]> = Vec::new();
        let _ = snap.scan(Perm::Spo, &[id.0, lf.0], |c| {
            push_quads(Perm::Spo, c, &mut quads);
            Ok(true)
        });
        for q in quads {
            let (Some(l), Some(gr)) = (literal(snap, q[2]), self.graph(g, q[3])) else {
                continue;
            };
            let lang = self.lang(&l.lang);
            let dt = self.datatype(&l.datatype);
            self.nodes[n as usize]
                .forms
                .push((l.text.into(), lang, dt, gr));
        }
    }

    /// Read every fact of the new entity `e` (the IRI with id `s`) from the snapshot.
    fn load_entity(
        &mut self,
        snap: &Snapshot,
        g: &mut Graphs<'_>,
        voc: &Vocab,
        s: Id,
        e: u32,
        fresh_nodes: &mut FxHashSet<u32>,
    ) -> Result<()> {
        let mut quads: Vec<[Id; 4]> = Vec::new();
        snap.scan(Perm::Spo, &[s.0], |c| {
            push_quads(Perm::Spo, c, &mut quads);
            Ok(true)
        })?;
        for q in quads {
            let Some(w) = voc.preds.get(&q[1]).copied() else {
                continue;
            };
            self.fact(snap, g, voc, &q, w, e, true, fresh_nodes);
        }
        Ok(())
    }

    /// Add (`add`) or remove fact `q` of entity `e`.
    #[allow(clippy::too_many_arguments)]
    fn fact(
        &mut self,
        snap: &Snapshot,
        g: &mut Graphs<'_>,
        voc: &Vocab,
        q: &[Id; 4],
        w: Watch,
        e: u32,
        add: bool,
        fresh_nodes: &mut FxHashSet<u32>,
    ) {
        let Some(graph) = self.graph(g, q[3]) else {
            return;
        };
        match w {
            Watch::Type => {
                let Some(&t) = voc.types.get(&q[2]) else {
                    return;
                };
                let types = &mut self.entities[e as usize].types;
                if add {
                    if !types.contains(&(graph, t)) {
                        types.push((graph, t));
                    }
                } else if let Some(i) = types.iter().position(|x| *x == (graph, t)) {
                    types.remove(i);
                }
            }
            Watch::Deprecated => {
                let Some(lit) = truthy(snap, q[2]) else {
                    return;
                };
                let d = &mut self.entities[e as usize].deprecated;
                if add {
                    d.push((graph, lit.into()));
                } else if let Some(i) = d.iter().position(|(gr, l)| *gr == graph && **l == *lit) {
                    d.remove(i);
                }
            }
            Watch::Prop(p) if PROPS[p as usize].xl => {
                let Some(key) = node_key(snap, q[2]) else {
                    return;
                };
                if add {
                    let (n, created) = self.node(&key);
                    if created {
                        self.load_forms(snap, g, voc, q[2], n);
                        fresh_nodes.insert(n);
                    }
                    self.link(e, p, graph, n);
                } else if let Some(&n) = self.node_ids.get(key.as_str()) {
                    self.unlink(e, p, graph, n);
                }
            }
            Watch::Prop(p) => {
                let Some(l) = literal(snap, q[2]) else {
                    return;
                };
                let lang = self.lang(&l.lang);
                let dt = self.datatype(&l.datatype);
                if add {
                    self.add_value(e, p, graph, NO_NODE, &l.text, lang, dt);
                } else {
                    self.remove_value(e, p, graph, NO_NODE, &l.text, lang, dt);
                }
            }
            Watch::LiteralForm => {}
        }
    }

    /// A literal form of a known node added or removed.
    fn form(&mut self, snap: &Snapshot, g: &mut Graphs<'_>, q: &[Id; 4], add: bool) {
        let Some(key) = node_key(snap, q[0]) else {
            return;
        };
        let Some(&n) = self.node_ids.get(key.as_str()) else {
            return;
        };
        let (Some(l), Some(graph)) = (literal(snap, q[2]), self.graph(g, q[3])) else {
            return;
        };
        let lang = self.lang(&l.lang);
        let dt = self.datatype(&l.datatype);
        let links = self.nodes[n as usize].links.clone();
        if add {
            self.nodes[n as usize]
                .forms
                .push((l.text.as_str().into(), lang, dt, graph));
            for (e, p, lg) in links {
                self.add_value(e, p, lg, n, &l.text, lang, dt);
            }
        } else {
            let forms = &mut self.nodes[n as usize].forms;
            let Some(i) = forms
                .iter()
                .position(|f| *f.0 == *l.text && f.1 == lang && f.2 == dt && f.3 == graph)
            else {
                return;
            };
            forms.remove(i);
            for (e, p, lg) in links {
                self.remove_value(e, p, lg, n, &l.text, lang, dt);
            }
        }
    }

    /// The index of `snap`, read from its type facts and then the watched properties.
    /// The index of `snap`. `check` runs between the scans and every 65,536 facts, and
    /// its error ends the build.
    pub(super) fn build(snap: &Snapshot, check: &dyn Fn() -> Result<()>) -> Result<Index> {
        let mut ix = Index::empty();
        ix.seq = snap.commit;
        let voc = Vocab::of(snap);
        let mut g = Graphs {
            snap,
            memo: FxHashMap::default(),
        };
        let Some(rdf_type) = voc
            .preds
            .iter()
            .find(|(_, w)| **w == Watch::Type)
            .map(|(id, _)| *id)
        else {
            return Ok(ix);
        };
        // the entities, by subject id
        let mut ids: FxHashMap<Id, u32> = FxHashMap::default();
        let mut types: Vec<(Id, u8)> = voc.types.iter().map(|(id, t)| (*id, *t)).collect();
        types.sort_by_key(|(_, t)| *t);
        let mut quads: Vec<[Id; 4]> = Vec::new();
        for (tid, t) in types {
            check()?;
            quads.clear();
            snap.scan(Perm::Pos, &[rdf_type.0, tid.0], |c| {
                push_quads(Perm::Pos, c, &mut quads);
                Ok(true)
            })?;
            for (i, q) in quads.iter().enumerate() {
                if i % 65_536 == 65_535 {
                    check()?;
                }
                let e = match ids.get(&q[0]) {
                    Some(&e) => e,
                    None => {
                        let Some(iri) = iri_of(snap, q[0]) else {
                            continue;
                        };
                        let (e, _) = ix.entity(&iri)?;
                        ids.insert(q[0], e);
                        e
                    }
                };
                let Some(graph) = ix.graph(&mut g, q[3]) else {
                    continue;
                };
                let ts = &mut ix.entities[e as usize].types;
                if !ts.contains(&(graph, t)) {
                    ts.push((graph, t));
                }
            }
        }
        // the facts of the entities, one watched predicate at a time
        let mut preds: Vec<(Id, Watch)> = voc
            .preds
            .iter()
            .filter(|(_, w)| !matches!(w, Watch::Type | Watch::LiteralForm))
            .map(|(id, w)| (*id, *w))
            .collect();
        preds.sort_by_key(|(_, w)| match w {
            Watch::Prop(p) => *p as u16,
            _ => 1000,
        });
        let mut fresh = FxHashSet::default();
        for (pid, w) in preds {
            check()?;
            quads.clear();
            snap.scan(Perm::Pos, &[pid.0], |c| {
                push_quads(Perm::Pos, c, &mut quads);
                Ok(true)
            })?;
            for (i, q) in quads.iter().enumerate() {
                if i % 65_536 == 65_535 {
                    check()?;
                }
                if let Some(&e) = ids.get(&q[0]) {
                    ix.fact(snap, &mut g, &voc, q, w, e, true, &mut fresh);
                }
            }
        }
        Ok(ix)
    }

    /// Apply the net changes of the commits after [`seq`](Self::seq) up to `snap`, as ids
    /// of `snap`. `false` when a term cannot be read, and the index must be built again.
    pub(super) fn apply(
        &mut self,
        snap: &Snapshot,
        added: &[[Id; 4]],
        removed: &[[Id; 4]],
    ) -> Result<bool> {
        let voc = Vocab::of(snap);
        let mut g = Graphs {
            snap,
            memo: FxHashMap::default(),
        };
        let watched = |q: &&[Id; 4]| voc.preds.contains_key(&q[1]);
        let removed: Vec<&[Id; 4]> = removed.iter().filter(watched).collect();
        let added: Vec<&[Id; 4]> = added.iter().filter(watched).collect();
        let mut subjects: FxHashMap<Id, Option<String>> = FxHashMap::default();
        let mut iri = |snap: &Snapshot, id: Id| -> Option<String> {
            subjects
                .entry(id)
                .or_insert_with(|| iri_of(snap, id))
                .clone()
        };
        let mut fresh_nodes = FxHashSet::default();
        // removals: type facts first, then the other facts
        let mut emptied: Vec<u32> = Vec::new();
        for q in &removed {
            if voc.preds[&q[1]] != Watch::Type {
                continue;
            }
            let Some(s) = iri(snap, q[0]) else { continue };
            let Some(&e) = self.by_iri.get(s.as_str()) else {
                continue;
            };
            self.fact(
                snap,
                &mut g,
                &voc,
                q,
                Watch::Type,
                e,
                false,
                &mut fresh_nodes,
            );
            if self.entities[e as usize].types.is_empty() {
                emptied.push(e);
            }
        }
        for q in &removed {
            match voc.preds[&q[1]] {
                Watch::Type => {}
                Watch::LiteralForm => self.form(snap, &mut g, q, false),
                w => {
                    let Some(s) = iri(snap, q[0]) else { continue };
                    if let Some(&e) = self.by_iri.get(s.as_str()) {
                        self.fact(snap, &mut g, &voc, q, w, e, false, &mut fresh_nodes);
                    }
                }
            }
        }
        for e in emptied {
            if self.entities[e as usize].types.is_empty() {
                self.drop_entity(e);
            }
        }
        // additions: new entities read their facts from the snapshot
        let mut fresh: FxHashSet<u32> = FxHashSet::default();
        for q in &added {
            if voc.preds[&q[1]] != Watch::Type || !voc.types.contains_key(&q[2]) {
                continue;
            }
            let Some(s) = iri(snap, q[0]) else { continue };
            let (e, created) = self.entity(&s)?;
            if created {
                fresh.insert(e);
                self.load_entity(snap, &mut g, &voc, q[0], e, &mut fresh_nodes)?;
            } else if !fresh.contains(&e) {
                self.fact(
                    snap,
                    &mut g,
                    &voc,
                    q,
                    Watch::Type,
                    e,
                    true,
                    &mut fresh_nodes,
                );
            }
        }
        // links before forms: a new node reads its forms, an old one takes them below
        let order = |w: Watch| match w {
            Watch::Prop(p) if PROPS[p as usize].xl => 0,
            Watch::LiteralForm => 1,
            _ => 2,
        };
        let mut rest: Vec<(&[Id; 4], Watch)> = added
            .iter()
            .map(|q| (*q, voc.preds[&q[1]]))
            .filter(|(_, w)| *w != Watch::Type)
            .collect();
        rest.sort_by_key(|(_, w)| order(*w));
        for (q, w) in rest {
            if w == Watch::LiteralForm {
                let Some(key) = node_key(snap, q[0]) else {
                    continue;
                };
                match self.node_ids.get(key.as_str()) {
                    Some(n) if !fresh_nodes.contains(n) => self.form(snap, &mut g, q, true),
                    _ => {}
                }
                continue;
            }
            let Some(s) = iri(snap, q[0]) else { continue };
            let Some(&e) = self.by_iri.get(s.as_str()) else {
                continue;
            };
            if !fresh.contains(&e) {
                self.fact(snap, &mut g, &voc, q, w, e, true, &mut fresh_nodes);
            }
        }
        self.seq = snap.commit;
        Ok(true)
    }

    /// Whether so many values are removed that a compaction pays.
    pub(super) fn wants_compaction(&self) -> bool {
        self.dead_values > 4096 && self.dead_values * 2 > self.values.len()
    }

    /// The index with the removed values and entities left out, rebuilt in memory.
    pub(super) fn compacted(&self) -> Index {
        let mut ix = Index::empty();
        ix.seq = self.seq;
        ix.graphs = self.graphs.clone();
        ix.graph_ids = self.graph_ids.clone();
        ix.langs = self.langs.clone();
        ix.lang_ids = self.lang_ids.clone();
        ix.datatypes = self.datatypes.clone();
        ix.datatype_ids = self.datatype_ids.clone();
        let mut node_map: FxHashMap<u32, u32> = FxHashMap::default();
        for node in &self.nodes {
            if node.key.is_empty() {
                continue;
            }
            let (n, _) = ix.node(&node.key);
            ix.nodes[n as usize].forms = node.forms.clone();
            node_map.insert(self.node_ids[&*node.key], n);
        }
        for ent in &self.entities {
            if ent.iri.is_empty() {
                continue;
            }
            let Ok((e, _)) = ix.entity(&ent.iri) else {
                continue;
            };
            let new = &mut ix.entities[e as usize];
            new.types = ent.types.clone();
            new.deprecated = ent.deprecated.clone();
            for &v in &ent.values {
                let x = &self.values[v as usize];
                if x.prop == LOCAL_NAME || !x.live() {
                    continue;
                }
                let node = if x.node == NO_NODE {
                    NO_NODE
                } else {
                    node_map[&x.node]
                };
                ix.add_value(e, x.prop, x.graph, node, &x.text, x.lang, x.datatype);
            }
            for &(p, gr, n) in &ent.links {
                let n = node_map[&n];
                ix.entities[e as usize].links.push((p, gr, n));
                ix.nodes[n as usize].links.push((e, p, gr));
            }
        }
        ix
    }

    /// An estimate of the bytes the index holds.
    pub(super) fn bytes(&self) -> usize {
        // a heap allocation's bookkeeping, and a B-tree entry's share of its node
        const ALLOC: usize = 16;
        const BTREE: usize = 8;
        let mut b = 0usize;
        b += self.values.capacity() * std::mem::size_of::<Value>();
        b += self
            .values
            .iter()
            .filter(|v| v.live())
            .map(|v| v.text.len() + ALLOC)
            .sum::<usize>();
        b += self.entities.capacity() * std::mem::size_of::<Entity>();
        for e in &self.entities {
            b += e.iri.len() + ALLOC;
            if e.values.spilled() {
                b += e.values.capacity() * 4 + ALLOC;
            }
            if e.types.spilled() {
                b += e.types.capacity() * 8 + ALLOC;
            }
            b += e.links.capacity() * 12 + e.deprecated.capacity() * 24;
        }
        for k in self.by_iri.keys() {
            b += k.len() + ALLOC + 16 + 4 + BTREE;
        }
        for (k, v) in self.labels.iter().chain(&self.alts) {
            b += k.len() + ALLOC + 16 + std::mem::size_of_val(v) + BTREE;
            if v.spilled() {
                b += v.capacity() * 4 + ALLOC;
            }
        }
        for (k, v) in &self.words {
            b += k.len() + ALLOC + 16 + 24 + v.capacity() * 4 + ALLOC + BTREE;
        }
        for n in &self.nodes {
            b += n.key.len() + n.forms.capacity() * 32 + n.links.capacity() * 12 + 3 * ALLOC;
        }
        b += self.node_ids.capacity() * 24;
        b
    }
}

#[cfg(test)]
impl Index {
    /// Every fact of the index as text, sorted, with the dictionaries' entries: two
    /// indexes of one state have equal dumps whatever their history.
    pub(super) fn dump(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let g = |n: u32| -> String {
            if n == ANY_GRAPH {
                "*".into()
            } else {
                self.graphs[n as usize].to_string()
            }
        };
        let node = |n: u32| -> String {
            if n == NO_NODE {
                String::new()
            } else {
                self.nodes[n as usize].key.to_string()
            }
        };
        let value = |v: u32| -> String {
            let x = &self.values[v as usize];
            format!(
                "{} {} {} {:?}@{}^^{} {}",
                self.entities[x.entity as usize].iri,
                x.prop,
                g(x.graph),
                x.text,
                self.langs[x.lang as usize],
                self.datatypes[x.datatype as usize],
                node(x.node)
            )
        };
        let mut live = 0;
        for (e, ent) in self.entities.iter().enumerate() {
            if ent.iri.is_empty() {
                continue;
            }
            live += 1;
            assert_eq!(self.by_iri.get(&*ent.iri), Some(&(e as u32)));
            for (gr, t) in &ent.types {
                out.push(format!("type {} {} {t}", ent.iri, g(*gr)));
            }
            for (gr, l) in &ent.deprecated {
                out.push(format!("deprecated {} {} {l}", ent.iri, g(*gr)));
            }
            for &v in &ent.values {
                assert!(self.values[v as usize].live());
                assert_eq!(self.values[v as usize].entity, e as u32);
                out.push(format!("value {}", value(v)));
            }
            for (p, gr, n) in &ent.links {
                out.push(format!("link {} {p} {} {}", ent.iri, g(*gr), node(*n)));
            }
        }
        assert_eq!(live, self.live_entities);
        assert_eq!(self.by_iri.len(), live);
        for (k, vs) in self.labels.iter().chain(&self.alts) {
            assert!(!vs.is_empty());
            for &v in vs {
                out.push(format!("full {} {}", String::from_utf8_lossy(k), value(v)));
            }
        }
        for (k, vs) in &self.words {
            assert!(!vs.is_empty());
            assert!(vs.windows(2).all(|w| w[0] < w[1]));
            for &v in vs {
                out.push(format!("word {} {}", String::from_utf8_lossy(k), value(v)));
            }
        }
        for n in &self.nodes {
            if n.key.is_empty() {
                continue;
            }
            assert!(!n.links.is_empty());
            for f in &n.forms {
                out.push(format!(
                    "form {} {:?}@{}^^{} {}",
                    n.key,
                    f.0,
                    self.langs[f.1 as usize],
                    self.datatypes[f.2 as usize],
                    g(f.3)
                ));
            }
        }
        out.sort();
        out
    }
}

/// Append the quads of a scan chunk.
fn push_quads(perm: Perm, c: Chunk<'_>, out: &mut Vec<[Id; 4]>) {
    match c {
        Chunk::Block(b, s, e) => out.extend((s..e).map(|i| perm.to_quad(&b.key(i)))),
        Chunk::Row(k) => out.push(perm.to_quad(&k)),
    }
}
