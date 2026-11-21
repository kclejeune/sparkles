//! The components of the data properties and which of them the checker takes (F24, "The
//! EL and Horn tiers").
//!
//! The data properties fall into components connected by the hierarchy and by
//! disjointness. A component whose data is within OWL 2 EL stays with the lattice of
//! `classify/data.rs`. Every other component, one with a universal restriction, a
//! cardinality, a datatype or literal outside the EL map that the checker reads, a facet,
//! a union, a complement, an enumeration of several values or a `DisjointDataProperties`
//! axiom, is all data atoms of the checker. A run chooses this once, before it normalizes
//! any axiom, from every axiom it holds, inside the profile or outside it. A range the
//! checker refuses, such as one with a pattern, an unknown datatype or an ill-typed
//! literal, makes nothing a checker component, since its axiom is left out either way.

use super::range::{Defs, Fail, Ranges};
use crate::classify::data::Kind;
use crate::model::*;
use crate::vocab;
use rustc_hash::FxHashMap;

/// The axioms the pre-pass reads between two polls of its stop.
const POLL: usize = 4096;

/// The components of a run's data properties.
#[derive(Clone, Debug, Default)]
pub struct Comps {
    /// the component of each data property the axioms name, top and bottom aside
    comp: FxHashMap<DataProperty, u32>,
    /// whether each component is the checker's
    checker: Vec<bool>,
}

impl Comps {
    /// The components of the data properties of `axioms`, with the datatype definitions
    /// that the data ranges naming a defined datatype stand for, or `None` once `stop`
    /// returns true.
    pub fn of(axioms: &[&AxiomKind], defs: &Defs, stop: &dyn Fn() -> bool) -> Option<Comps> {
        let mut w = Walk {
            ids: FxHashMap::default(),
            parent: Vec::new(),
            trig: Vec::new(),
            defs: defs.unconverted(),
            el_defs: FxHashMap::default(),
            scratch: Ranges::new(),
            stop,
            stopped: false,
        };
        for (i, &k) in axioms.iter().enumerate() {
            if w.stopped || (i.is_multiple_of(POLL) && stop()) {
                return None;
            }
            w.axiom(k);
        }
        if w.stopped {
            return None;
        }
        let n = w.parent.len();
        let mut root_comp: FxHashMap<usize, u32> = FxHashMap::default();
        let mut checker = Vec::new();
        let mut of_id = vec![0u32; n];
        for (i, slot) in of_id.iter_mut().enumerate() {
            let r = w.find(i);
            let next = root_comp.len() as u32;
            let c = *root_comp.entry(r).or_insert_with(|| {
                checker.push(false);
                next
            });
            *slot = c;
        }
        for i in 0..n {
            if w.trig[i] {
                checker[of_id[i] as usize] = true;
            }
        }
        let comp = w.ids.into_iter().map(|(d, i)| (d, of_id[i])).collect();
        Some(Comps { comp, checker })
    }

    /// The component of a data property, when the axioms name it.
    pub fn component(&self, d: &DataProperty) -> Option<u32> {
        self.comp.get(d).copied()
    }

    /// Whether the checker takes the data property's component.
    pub fn checker(&self, d: &DataProperty) -> bool {
        self.component(d).is_some_and(|c| self.checker[c as usize])
    }

    /// Whether any component is the checker's.
    pub fn any(&self) -> bool {
        self.checker.iter().any(|&c| c)
    }

    /// The number of components.
    pub fn len(&self) -> usize {
        self.checker.len()
    }

    pub fn is_empty(&self) -> bool {
        self.checker.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.comp.capacity() * (std::mem::size_of::<(DataProperty, u32)>() + 1)
            + self.checker.capacity()
    }
}

fn is_special(d: &DataProperty) -> bool {
    matches!(
        d.as_str(),
        vocab::OWL_TOP_DATA_PROPERTY | vocab::OWL_BOTTOM_DATA_PROPERTY
    )
}

struct Walk<'a> {
    ids: FxHashMap<DataProperty, usize>,
    parent: Vec<usize>,
    trig: Vec<bool>,
    defs: Defs,
    /// whether each defined datatype has the forms of OWL 2 EL, `None` while it is read
    el_defs: FxHashMap<Datatype, Option<bool>>,
    scratch: Ranges,
    stop: &'a dyn Fn() -> bool,
    stopped: bool,
}

impl Walk<'_> {
    fn id(&mut self, d: &DataProperty) -> Option<usize> {
        if is_special(d) {
            return None;
        }
        if let Some(&i) = self.ids.get(d) {
            return Some(i);
        }
        let i = self.parent.len();
        self.parent.push(i);
        self.trig.push(false);
        self.ids.insert(d.clone(), i);
        Some(i)
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn join(&mut self, a: &DataProperty, b: &DataProperty) {
        let (Some(a), Some(b)) = (self.id(a), self.id(b)) else {
            // an edge to the top or bottom property would join every component
            return;
        };
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.parent[a] = b;
        }
    }

    fn mark(&mut self, d: &DataProperty) {
        if let Some(i) = self.id(d) {
            self.trig[i] = true;
        }
    }

    /// Names the property, so that it has a component.
    fn note(&mut self, d: &DataProperty) {
        self.id(d);
    }

    fn axiom(&mut self, k: &AxiomKind) {
        use AxiomKind as K;
        match k {
            K::SubClassOf(a, b) => {
                self.class(a);
                self.class(b);
            }
            K::EquivalentClasses(cs) | K::DisjointClasses(cs) => {
                cs.iter().for_each(|c| self.class(c))
            }
            K::DisjointUnion(_, cs) => cs.iter().for_each(|c| self.class(c)),
            K::ObjectPropertyDomain(_, c) | K::ObjectPropertyRange(_, c) => self.class(c),
            K::ClassAssertion(c, _) => self.class(c),
            K::SubDataPropertyOf(a, b) => self.join(a, b),
            K::EquivalentDataProperties(ps) => {
                let ps: Vec<&DataProperty> = ps.iter().collect();
                ps.iter().for_each(|p| self.note(p));
                for w in ps.windows(2) {
                    self.join(w[0], w[1]);
                }
            }
            K::DisjointDataProperties(ps) => {
                let ps: Vec<&DataProperty> = ps.iter().collect();
                for p in &ps {
                    self.mark(p);
                }
                for w in ps.windows(2) {
                    self.join(w[0], w[1]);
                }
            }
            K::DataPropertyDomain(d, c) => {
                self.note(d);
                self.class(c);
            }
            K::DataPropertyRange(d, r) => {
                if self.range_triggers(r) {
                    self.mark(d);
                } else {
                    self.note(d);
                }
            }
            K::FunctionalDataProperty(d) => self.note(d),
            K::HasKey(c, _, ds) => {
                self.class(c);
                ds.iter().for_each(|d| self.note(d));
            }
            K::DataPropertyAssertion(d, _, l) | K::NegativeDataPropertyAssertion(d, _, l) => {
                if self.literal_triggers(l) {
                    self.mark(d);
                } else {
                    self.note(d);
                }
            }
            _ => {}
        }
    }

    fn class(&mut self, c: &ClassExpression) {
        use ClassExpression as C;
        match c {
            C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
                cs.iter().for_each(|c| self.class(c))
            }
            C::ObjectComplementOf(c)
            | C::ObjectSomeValuesFrom(_, c)
            | C::ObjectAllValuesFrom(_, c) => self.class(c),
            C::ObjectMinCardinality(_, _, Some(c))
            | C::ObjectMaxCardinality(_, _, Some(c))
            | C::ObjectExactCardinality(_, _, Some(c)) => self.class(c),
            C::DataSomeValuesFrom(ps, r) if ps.len() == 1 => {
                if self.range_triggers(r) {
                    self.mark(&ps[0]);
                } else {
                    self.note(&ps[0]);
                }
            }
            C::DataHasValue(d, l) => {
                if self.literal_triggers(l) {
                    self.mark(d);
                } else {
                    self.note(d);
                }
            }
            C::DataAllValuesFrom(ps, _) if ps.len() == 1 => self.mark(&ps[0]),
            C::DataMinCardinality(_, d, _)
            | C::DataMaxCardinality(_, d, _)
            | C::DataExactCardinality(_, d, _) => self.mark(d),
            _ => {}
        }
    }

    /// Whether a literal is outside the EL map and has a value the checker reads.
    fn literal_triggers(&mut self, l: &Literal) -> bool {
        !el_literal(l) && self.scratch.literal(l).is_ok()
    }

    /// Whether a data range is outside the EL forms and the checker reads it.
    fn range_triggers(&mut self, r: &DataRange) -> bool {
        if self.el_range(r, 0) {
            return false;
        }
        match self.scratch.of_defined(r, &mut self.defs, self.stop) {
            Ok(_) => true,
            Err(Fail::Unsupported(_)) => false,
            Err(Fail::Stopped) => {
                self.stopped = true;
                false
            }
        }
    }

    /// Whether a data range has the forms of OWL 2 EL: datatypes of the EL map,
    /// intersections and enumerations of one literal of the map. Each definition is read
    /// once.
    fn el_range(&mut self, r: &DataRange, depth: usize) -> bool {
        if depth > Defs::MAX_DEPTH {
            return false;
        }
        match r {
            DataRange::Datatype(d) => {
                let iri = d.0.as_str();
                if iri == vocab::RDFS_LITERAL || Kind::of(iri).is_some() {
                    return true;
                }
                let Some(x) = self.defs.map().get(d).cloned() else {
                    return false;
                };
                match self.el_defs.get(d) {
                    Some(Some(b)) => *b,
                    // a definition that names itself
                    Some(None) => false,
                    None => {
                        self.el_defs.insert(d.clone(), None);
                        let b = self.el_range(&x, depth + 1);
                        self.el_defs.insert(d.clone(), Some(b));
                        b
                    }
                }
            }
            DataRange::IntersectionOf(rs) => rs.iter().all(|x| self.el_range(x, depth + 1)),
            DataRange::OneOf(ls) => ls.len() == 1 && ls.iter().all(el_literal),
            _ => false,
        }
    }
}

impl Comps {
    /// The checker components whose properties the axiom names.
    pub fn touched(&self, k: &AxiomKind) -> Vec<u32> {
        let mut out = Vec::new();
        props_of(k, &mut |d| {
            if let Some(c) = self.component(d)
                && self.checker[c as usize]
                && !out.contains(&c)
            {
                out.push(c);
            }
        });
        out
    }
}

/// Calls `f` on each data property that the axiom names.
fn props_of(k: &AxiomKind, f: &mut dyn FnMut(&DataProperty)) {
    use AxiomKind as K;
    match k {
        K::SubClassOf(a, b) => {
            class_props(a, f);
            class_props(b, f);
        }
        K::EquivalentClasses(cs) | K::DisjointClasses(cs) | K::DisjointUnion(_, cs) => {
            cs.iter().for_each(|c| class_props(c, f))
        }
        K::ObjectPropertyDomain(_, c) | K::ObjectPropertyRange(_, c) | K::ClassAssertion(c, _) => {
            class_props(c, f)
        }
        K::SubDataPropertyOf(a, b) => {
            f(a);
            f(b);
        }
        K::EquivalentDataProperties(ps) | K::DisjointDataProperties(ps) => {
            ps.iter().for_each(&mut *f)
        }
        K::DataPropertyDomain(d, c) => {
            f(d);
            class_props(c, f);
        }
        K::DataPropertyRange(d, _)
        | K::FunctionalDataProperty(d)
        | K::DataPropertyAssertion(d, _, _)
        | K::NegativeDataPropertyAssertion(d, _, _) => f(d),
        K::HasKey(c, _, ds) => {
            class_props(c, f);
            ds.iter().for_each(&mut *f);
        }
        _ => {}
    }
}

fn class_props(c: &ClassExpression, f: &mut dyn FnMut(&DataProperty)) {
    use ClassExpression as C;
    match c {
        C::ObjectIntersectionOf(cs) | C::ObjectUnionOf(cs) => {
            cs.iter().for_each(|c| class_props(c, f))
        }
        C::ObjectComplementOf(c) | C::ObjectSomeValuesFrom(_, c) | C::ObjectAllValuesFrom(_, c) => {
            class_props(c, f)
        }
        C::ObjectMinCardinality(_, _, Some(c))
        | C::ObjectMaxCardinality(_, _, Some(c))
        | C::ObjectExactCardinality(_, _, Some(c)) => class_props(c, f),
        C::DataSomeValuesFrom(ps, _) | C::DataAllValuesFrom(ps, _) => ps.iter().for_each(&mut *f),
        C::DataHasValue(d, _)
        | C::DataMinCardinality(_, d, _)
        | C::DataMaxCardinality(_, d, _)
        | C::DataExactCardinality(_, d, _) => f(d),
        _ => {}
    }
}

/// Whether a literal's datatype is in the EL map, or it has a language tag.
fn el_literal(l: &Literal) -> bool {
    l.0.language().is_some() || Kind::of(l.0.datatype().as_str()).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ofn_read;

    fn comps(axioms: &str) -> Comps {
        let doc = ofn_read::parse(&format!(
            "Prefix(:=<http://ex.org/>)\nPrefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
             Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
             Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)\n\
             Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\nOntology(\n{axioms}\n)"
        ))
        .unwrap();
        let ax: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let mut defs = FxHashMap::default();
        for k in &ax {
            if let AxiomKind::DatatypeDefinition(d, r) = k {
                defs.insert(d.clone(), r.clone());
            }
        }
        Comps::of(&ax, &Defs::new(defs), &|| false).unwrap()
    }

    fn d(name: &str) -> DataProperty {
        DataProperty::new(format!("http://ex.org/{name}"))
    }

    #[test]
    fn el_data_stays_with_the_lattice() {
        let c = comps(
            "SubClassOf(:A DataSomeValuesFrom(:d xsd:integer))\n\
             SubClassOf(:B DataHasValue(:e \"x\"))\n\
             DataPropertyRange(:d DataIntersectionOf(xsd:decimal xsd:integer))\n\
             SubClassOf(:A DataSomeValuesFrom(:d DataOneOf(\"1\"^^xsd:integer)))\n\
             FunctionalDataProperty(:d)",
        );
        assert!(!c.any());
        assert_eq!(c.len(), 2);
        assert!(!c.is_empty() && c.bytes() > 0);
        assert!(c.component(&d("d")).is_some());
        assert_ne!(c.component(&d("d")), c.component(&d("e")));
    }

    #[test]
    fn each_kind_of_trigger_marks_its_component() {
        for ax in [
            "SubClassOf(:A DataAllValuesFrom(:d xsd:integer))",
            "SubClassOf(:A DataMinCardinality(2 :d))",
            "SubClassOf(:A DataMaxCardinality(1 :d xsd:string))",
            "SubClassOf(:A DataExactCardinality(1 :d))",
            "SubClassOf(:A DataSomeValuesFrom(:d xsd:int))",
            "SubClassOf(:A DataSomeValuesFrom(:d xsd:boolean))",
            "SubClassOf(:A DataSomeValuesFrom(:d DatatypeRestriction(xsd:integer xsd:minInclusive \"1\"^^xsd:integer)))",
            "SubClassOf(:A DataSomeValuesFrom(:d DataUnionOf(xsd:integer xsd:string)))",
            "SubClassOf(:A DataSomeValuesFrom(:d DataComplementOf(xsd:integer)))",
            "SubClassOf(:A DataSomeValuesFrom(:d DataOneOf(\"1\"^^xsd:integer \"2\"^^xsd:integer)))",
            "SubClassOf(:A DataHasValue(:d \"1\"^^xsd:int))",
            "DataPropertyRange(:d xsd:double)",
            "DataPropertyAssertion(:d :a \"true\"^^xsd:boolean)",
            "NegativeDataPropertyAssertion(:d :a \"1.5\"^^xsd:float)",
            "DisjointDataProperties(:d :e)",
            "SubClassOf(ObjectSomeValuesFrom(:r DataAllValuesFrom(:d xsd:integer)) :A)",
        ] {
            let c = comps(&format!(
                "{ax}\nSubClassOf(:A DataSomeValuesFrom(:d xsd:integer))"
            ));
            assert!(c.checker(&d("d")), "{ax}");
        }
    }

    #[test]
    fn refused_ranges_trigger_nothing() {
        for ax in [
            "SubClassOf(:A DataSomeValuesFrom(:d DatatypeRestriction(xsd:string xsd:pattern \"a*\")))",
            "SubClassOf(:A DataSomeValuesFrom(:d <http://ex.org/unknown>))",
            "SubClassOf(:A DataHasValue(:d \"x\"^^xsd:int))",
            "DataPropertyRange(:d rdf:XMLLiteral)",
        ] {
            let c = comps(ax);
            assert!(!c.checker(&d("d")), "{ax}");
        }
    }

    #[test]
    fn the_hierarchy_and_disjointness_join_components() {
        let c = comps(
            "SubDataPropertyOf(:d :e)\n\
             EquivalentDataProperties(:e :f)\n\
             SubClassOf(:A DataSomeValuesFrom(:f xsd:int))\n\
             SubClassOf(:A DataSomeValuesFrom(:g xsd:integer))\n\
             DisjointDataProperties(:h :i)",
        );
        assert!(c.checker(&d("d")) && c.checker(&d("e")) && c.checker(&d("f")));
        assert!(!c.checker(&d("g")));
        assert!(c.checker(&d("h")) && c.checker(&d("i")));
        assert_eq!(c.component(&d("h")), c.component(&d("i")));
        assert_ne!(c.component(&d("d")), c.component(&d("h")));
    }

    #[test]
    fn edges_to_the_top_and_bottom_properties_join_nothing() {
        let c = comps(
            "SubDataPropertyOf(:d owl:topDataProperty)\n\
             SubDataPropertyOf(:e owl:topDataProperty)\n\
             SubDataPropertyOf(:g owl:bottomDataProperty)\n\
             SubClassOf(:A DataSomeValuesFrom(:d xsd:int))\n\
             SubClassOf(:A DataSomeValuesFrom(:e xsd:integer))",
        );
        assert!(c.checker(&d("d")));
        assert!(!c.checker(&d("e")));
        assert!(c.component(&DataProperty::top()).is_none());
    }

    #[test]
    fn a_defined_datatype_stands_for_its_definition() {
        let c = comps(
            "DatatypeDefinition(:small DatatypeRestriction(xsd:integer xsd:maxInclusive \"5\"^^xsd:integer))\n\
             DatatypeDefinition(:num xsd:integer)\n\
             SubClassOf(:A DataSomeValuesFrom(:d :small))\n\
             SubClassOf(:A DataSomeValuesFrom(:e :num))",
        );
        assert!(c.checker(&d("d")));
        assert!(!c.checker(&d("e")));
    }

    #[test]
    fn shared_nested_definitions_are_read_once() {
        // each definition names the one below it twice, so its expansion has 2^40 leaves
        let mut ax = String::from(
            "DatatypeDefinition(:t0 DatatypeRestriction(xsd:integer xsd:maxInclusive \"5\"^^xsd:integer))\n",
        );
        for i in 1..=40 {
            ax.push_str(&format!(
                "DatatypeDefinition(:t{i} DataIntersectionOf(:t{p} DataUnionOf(:t{p} xsd:string)))\n",
                p = i - 1
            ));
        }
        ax.push_str("SubClassOf(:A DataSomeValuesFrom(:d :t40))\nSubClassOf(:A DataSomeValuesFrom(:e xsd:integer))");
        let c = comps(&ax);
        assert!(c.checker(&d("d")));
        assert!(!c.checker(&d("e")));
    }

    #[test]
    fn a_definition_that_names_itself_triggers_nothing() {
        let c = comps(
            "DatatypeDefinition(:loop DataUnionOf(:loop xsd:string))\n\
             SubClassOf(:A DataSomeValuesFrom(:d :loop))",
        );
        assert!(!c.checker(&d("d")));
    }

    #[test]
    fn the_pre_pass_stops_on_its_flag() {
        let doc = ofn_read::parse(
            "Prefix(:=<http://ex.org/>)\nPrefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
             Ontology(\nSubClassOf(:A DataSomeValuesFrom(:d xsd:int))\n)",
        )
        .unwrap();
        let ax: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        assert!(Comps::of(&ax, &Defs::default(), &|| true).is_none());
    }
}
