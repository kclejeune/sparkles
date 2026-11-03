//! `reason`: the ontology with the axioms its classification infers, as ROBOT 1.9.11's
//! `ReasonOperation` writes it with ELK.

use super::{
    BOTTOM, Classify, Document, Error, Report, Signature, View, fresh_class, nothing, thing,
};
use crate::classify::Classification;
use crate::model::*;
use crate::vocab;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeSet;

/// The inferred axiom generators of `--axiom-generators`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generator {
    /// `SubClassOf(c d)` for each superclass d of each class c, direct ones only unless
    /// `--include-indirect`
    SubClass,
    /// `EquivalentClasses(c d …)` for each class with equivalent classes
    EquivalentClass,
    /// `ClassAssertion(c a)` for each type c of each named individual a, direct ones only
    /// unless `--include-indirect`
    ClassAssertion,
    /// a generator whose queries ELK does not answer, which ROBOT logs and skips: the
    /// label OWL API gives it
    Unanswered(&'static str),
}

impl Generator {
    /// Parse ROBOT's space-separated list, by the names of `ReasonerHelper`, in any case.
    /// The empty name, as an empty list gives, is `subclass`.
    pub fn parse_list(s: &str) -> Result<Vec<Generator>, String> {
        // Java's split(" "): empty names between spaces are kept, trailing ones dropped
        let mut names: Vec<&str> = s.split(' ').collect();
        while names.len() > 1 && names.last() == Some(&"") {
            names.pop();
        }
        names.into_iter().map(Generator::parse).collect()
    }

    pub fn parse(name: &str) -> Result<Generator, String> {
        use Generator::*;
        Ok(match name.to_lowercase().as_str() {
            "subclass" | "" => SubClass,
            "equivalentclass" => EquivalentClass,
            "classassertion" => ClassAssertion,
            "disjointclasses" => Unanswered("Disjoint classes"),
            "datapropertycharacteristic" => Unanswered("Data property characteristics"),
            "equivalentdataproperties" => Unanswered("Equivalent data properties"),
            "subdataproperty" => Unanswered("Sub data properties"),
            "propertyassertion" => Unanswered("Property assertions (property values)"),
            "equivalentobjectproperty" => Unanswered("Equivalent object properties"),
            "inverseobjectproperties" => Unanswered("Inverse object properties"),
            "objectpropertycharacteristic" => Unanswered("Object property characteristics"),
            "objectpropertyrange" => Unanswered("Object property ranges"),
            "objectpropertydomain" => Unanswered("Object property domains"),
            "subobjectproperty" => Unanswered("Sub object properties"),
            _ => {
                return Err(format!(
                    "reason#UNKNOWN AXIOM GENERATOR {name} is not a valid inferred axiom generator"
                ));
            }
        })
    }
}

/// `--equivalent-classes-allowed`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EquivalentClasses {
    #[default]
    All,
    None,
    AssertedOnly,
}

impl EquivalentClasses {
    /// ROBOT's values: `all` or `true`, `none` or `false`, `asserted-only`.
    pub fn parse(s: &str) -> Result<EquivalentClasses, String> {
        match s {
            "all" | "true" => Ok(EquivalentClasses::All),
            "none" | "false" => Ok(EquivalentClasses::None),
            "asserted-only" => Ok(EquivalentClasses::AssertedOnly),
            _ => Err(format!(
                "reason#EQUIVALENT CLASSES OPTION ERROR 'equivalent-classes-allowed' value {s} must be one of: true, false, all, none, asserted-only"
            )),
        }
    }

    pub fn explanation(self) -> &'static str {
        match self {
            EquivalentClasses::All => "All equivalent class axioms are allowed",
            EquivalentClasses::None => "No equivalent class axioms are allowed",
            EquivalentClasses::AssertedOnly => {
                "Only equivalent classes that have been asserted are allowed. Inferred equivalencies are forbidden."
            }
        }
    }
}

/// `--exclude-tautologies`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tautologies {
    #[default]
    Keep,
    /// ROBOT's hard-coded patterns
    Structural,
    /// the axioms that the empty ontology entails, which ROBOT asks HermiT. For the
    /// axioms between named entities that the generators write, these are exactly the
    /// structural patterns.
    All,
}

impl Tautologies {
    /// ROBOT's reading: `false` in any case keeps them, `structural` in any case is the
    /// patterns, and any other value asks for all of them.
    pub fn parse(s: &str) -> Tautologies {
        if s.eq_ignore_ascii_case("false") {
            Tautologies::Keep
        } else if s.eq_ignore_ascii_case("structural") {
            Tautologies::Structural
        } else {
            Tautologies::All
        }
    }
}

/// `--create-new-ontology` and `--create-new-ontology-with-annotations`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NewOntology {
    #[default]
    No,
    /// the inferred axioms alone
    Empty,
    /// the inferred axioms and the annotation assertions
    WithAnnotations,
}

/// The options of `reason`, with ROBOT's defaults.
#[derive(Clone, Debug)]
pub struct ReasonOptions {
    pub generators: Vec<Generator>,
    pub include_indirect: bool,
    pub equivalent_classes: EquivalentClasses,
    pub tautologies: Tautologies,
    pub exclude_duplicate_axioms: bool,
    pub exclude_owl_thing: bool,
    pub exclude_external_entities: bool,
    pub remove_redundant_subclass_axioms: bool,
    pub preserve_annotated_axioms: bool,
    pub annotate_inferred_axioms: bool,
    pub new_ontology: NewOntology,
    /// compute the module of the unsatisfiable classes when there are any
    pub dump_unsatisfiable: bool,
}

impl Default for ReasonOptions {
    fn default() -> ReasonOptions {
        ReasonOptions {
            generators: vec![Generator::SubClass],
            include_indirect: false,
            equivalent_classes: EquivalentClasses::All,
            tautologies: Tautologies::Keep,
            exclude_duplicate_axioms: false,
            exclude_owl_thing: false,
            exclude_external_entities: false,
            remove_redundant_subclass_axioms: true,
            preserve_annotated_axioms: false,
            annotate_inferred_axioms: false,
            new_ontology: NewOntology::No,
            dump_unsatisfiable: false,
        }
    }
}

/// The annotation `reason --annotate-inferred-axioms` puts on an inferred axiom.
pub const IS_INFERRED: &str = "http://www.geneontology.org/formats/oboInOwl#is_inferred";

/// The fresh classes' IRIs: a probe of an object property, and a class of one individual.
const PROBE: &str = "urn:sparkles:reason:probe:";
const NOMINAL: &str = "urn:sparkles:reason:individual:";

/// ROBOT's check for unsatisfiable object properties with ELK: a fresh class under
/// `ObjectSomeValuesFrom(p owl:Thing)` for each property of the signature, which no other
/// class can be below or equal to, so it changes no answer about the other classes and
/// joins the classification the command runs anyway.
pub(crate) struct Probes {
    of: Vec<(Class, ObjectProperty)>,
    pub axioms: Vec<AxiomKind>,
}

impl Probes {
    /// The probes of the signature's object properties, each fresh class added to `fresh`.
    pub fn new(sig: &Signature, fresh: &mut FxHashSet<Class>) -> Probes {
        let mut of: Vec<(Class, ObjectProperty)> = Vec::new();
        for (i, p) in sig.object_properties.iter().enumerate() {
            let c = fresh_class(PROBE, i, &sig.classes);
            fresh.insert(c.clone());
            of.push((c, p.clone()));
        }
        let axioms = of
            .iter()
            .map(|(c, p)| {
                AxiomKind::SubClassOf(
                    ClassExpression::Class(c.clone()),
                    ClassExpression::some(p.clone(), thing()),
                )
            })
            .collect();
        Probes { of, axioms }
    }

    /// ROBOT's `ReasonerHelper.validate`: an inconsistent ontology, then the unsatisfiable
    /// classes that `is_fresh` does not name, then the object properties whose probe is
    /// unsatisfiable, fail. `dump` gives the document whose bottom-locality module of the
    /// unsatisfiable classes the error carries, when one is asked for.
    pub fn validate<'d, E>(
        &self,
        classification: &Classification,
        is_fresh: impl Fn(&Class) -> bool,
        dump: impl FnOnce() -> Option<&'d Document>,
    ) -> Result<(), Error<E>> {
        if !classification.consistent {
            return Err(Error::Inconsistent);
        }
        let unsat: Vec<Class> = classification
            .taxonomy
            .node(BOTTOM)
            .classes
            .iter()
            .filter(|c| c.as_str() != vocab::OWL_NOTHING && !is_fresh(c))
            .cloned()
            .collect();
        if !unsat.is_empty() {
            let module = dump().map(|d| super::bottom_module(d.axioms.iter(), unsat.iter()));
            return Err(Error::UnsatisfiableClasses {
                classes: unsat,
                module,
            });
        }
        let unsat_props: Vec<ObjectProperty> = self
            .of
            .iter()
            .filter(|(c, _)| classification.taxonomy.is_unsatisfiable(c))
            .map(|(_, p)| p.clone())
            .collect();
        if !unsat_props.is_empty() {
            return Err(Error::UnsatisfiableProperties(unsat_props));
        }
        Ok(())
    }
}

/// Classify the document and assert the inferred axioms, as ROBOT's `reason` does. The
/// document is changed only when the run succeeds.
pub fn reason<E>(
    doc: &mut Document,
    o: &ReasonOptions,
    classify: &mut Classify<'_, E>,
) -> Result<Report, Error<E>> {
    let mut report = Report::default();
    let sig = doc.signature();

    let mut fresh: FxHashSet<Class> = FxHashSet::default();
    let probes = Probes::new(&sig, &mut fresh);
    let mut kinds = doc.logical_kinds();
    kinds.extend(probes.axioms.iter());
    let (classification, excluded) = classify(&kinds).map_err(Error::Classifier)?;
    drop(kinds);
    report.excluded = excluded;
    let view = View::new(&classification, &fresh);
    let input: &Document = doc;
    probes.validate(
        &classification,
        |c| fresh.contains(c),
        || o.dump_unsatisfiable.then_some(input),
    )?;

    // the equivalences between named classes, as `EquivalentClassReasoning` finds them
    if o.equivalent_classes != EquivalentClasses::All {
        let asserted: FxHashSet<&BTreeSet<ClassExpression>> = doc
            .axioms
            .iter()
            .filter_map(|a| match &a.kind {
                AxiomKind::EquivalentClasses(cs) => Some(cs),
                _ => None,
            })
            .collect();
        let mut failing: BTreeSet<(Class, Class)> = BTreeSet::new();
        for c in &sig.classes {
            let Some(n) = view.node_of(c) else { continue };
            for d in view.members(n).filter(|d| *d != c) {
                let pair: BTreeSet<ClassExpression> = [c.clone().into(), d.clone().into()].into();
                let fails = match o.equivalent_classes {
                    EquivalentClasses::None => true,
                    _ => !asserted.contains(&pair),
                };
                if fails {
                    let (a, b) = if c < d { (c, d) } else { (d, c) };
                    failing.insert((a.clone(), b.clone()));
                }
            }
        }
        if !failing.is_empty() {
            return Err(Error::EquivalentClasses {
                mode: o.equivalent_classes,
                pairs: failing.into_iter().collect(),
            });
        }
    }

    // the inferred axioms of each generator, as OWL API's InferredOntologyGenerator
    // fills a new ontology with them
    let mut inferred: BTreeSet<Axiom> = BTreeSet::new();
    for g in &o.generators {
        match g {
            Generator::SubClass => {
                for c in &sig.classes {
                    if view.node_of(c) == Some(BOTTOM) {
                        // owl:Nothing, the only unsatisfiable class left
                        inferred.insert(Axiom::sub_class_of(c.clone(), nothing()));
                        continue;
                    }
                    let nodes = if o.include_indirect {
                        view.strict_super_nodes(c)
                    } else {
                        view.direct_super_nodes(c)
                    };
                    for n in nodes {
                        for d in view.members(n) {
                            inferred.insert(Axiom::sub_class_of(c.clone(), d.clone()));
                        }
                    }
                }
            }
            Generator::EquivalentClass => {
                for c in &sig.classes {
                    let Some(n) = view.node_of(c) else { continue };
                    let mut cs: BTreeSet<ClassExpression> =
                        view.members(n).map(|d| d.clone().into()).collect();
                    cs.insert(c.clone().into());
                    if cs.len() > 1 {
                        inferred.insert(Axiom::from(AxiomKind::EquivalentClasses(cs)));
                    }
                }
            }
            Generator::ClassAssertion => {
                let types = individual_types(doc, &sig, o.include_indirect, classify)?;
                report.warnings.extend(types.1);
                inferred.extend(types.0);
            }
            Generator::Unanswered(label) => report.warnings.push(format!(
                "the classifier answers no queries for {label}, as ELK answers none: no such axioms were generated"
            )),
        }
    }

    // maybeCreateNewOntology
    let mut out = doc.clone();
    match o.new_ontology {
        NewOntology::No => {}
        NewOntology::Empty => {
            out.axioms.clear();
            out.header.imports.clear();
        }
        NewOntology::WithAnnotations => {
            out.axioms
                .retain(|a| matches!(a.kind, AxiomKind::AnnotationAssertion(..)));
            out.header.imports.clear();
        }
    }

    // addInferredAxioms
    let declared: FxHashSet<&Entity> = out
        .axioms
        .iter()
        .filter_map(|a| match &a.kind {
            AxiomKind::Declaration(e) => Some(e),
            _ => None,
        })
        .collect();
    let mut keep: Vec<Axiom> = Vec::new();
    for a in inferred {
        if o.exclude_external_entities && is_class_axiom(&a.kind) {
            let classes = classes_of(&a.kind);
            if !classes
                .iter()
                .any(|c| declared.contains(&Entity::Class(c.clone())))
            {
                continue;
            }
        }
        if o.exclude_duplicate_axioms && out.axioms.contains(&a) {
            continue;
        }
        if (o.exclude_duplicate_axioms || o.exclude_owl_thing) && mentions_thing(&a.kind) {
            continue;
        }
        if o.tautologies != Tautologies::Keep && is_tautology(&a.kind) {
            continue;
        }
        keep.push(a);
    }
    drop(declared);
    let annotation = Annotation::new(
        AnnotationProperty::new(IS_INFERRED),
        AnnotationValue::Literal(Literal::simple("true")),
    );
    for a in keep {
        if o.annotate_inferred_axioms {
            // OntologyHelper.addAxiomAnnotation replaces the axiom, asserted or not, by
            // the annotated one, and refuses every type but SubClassOf
            if !matches!(a.kind, AxiomKind::SubClassOf(..)) {
                return Err(Error::Refused(format!(
                    "ontology helper#AXIOM TYPE ERROR cannot annotate axioms of type: {}",
                    a.axiom_type().name()
                )));
            }
            out.axioms.remove(&a);
            if out.axioms.insert(a.annotated(annotation.clone())) {
                report.added += 1;
            }
        } else if out.axioms.insert(a) {
            report.added += 1;
        }
    }

    // removeRedundantSubClassAxioms, with the classification of the input ontology
    if o.remove_redundant_subclass_axioms {
        let mut supers: FxHashMap<&Class, FxHashSet<&Class>> = FxHashMap::default();
        let mut remove: Vec<Axiom> = Vec::new();
        for a in &out.axioms {
            let AxiomKind::SubClassOf(ClassExpression::Class(c), ClassExpression::Class(d)) =
                &a.kind
            else {
                continue;
            };
            // only the axiom without annotations is removed, so an annotated one stays
            // whether or not --preserve-annotated-axioms protects it
            if !a.annotations.is_empty()
                || c.as_str() == vocab::OWL_THING
                || c.as_str() == vocab::OWL_NOTHING
            {
                continue;
            }
            let direct = supers.entry(c).or_insert_with(|| {
                view.direct_super_nodes(c)
                    .into_iter()
                    .flat_map(|n| view.members(n))
                    .collect()
            });
            if !direct.contains(d) {
                remove.push(a.clone());
            }
        }
        drop(supers);
        report.removed = remove.len();
        for a in remove {
            out.axioms.remove(&a);
        }
    }
    let _ = o.preserve_annotated_axioms;
    *doc = out;
    Ok(report)
}

/// The class assertions of each named individual's types, from a classification with a
/// fresh class equivalent to `ObjectOneOf(a)` for each individual a: the classes of its
/// node, or of its parents' nodes when it has none, as ELK's `getTypes(a, true)`, or the
/// classes of its node and every node above with `indirect`.
fn individual_types<E>(
    doc: &Document,
    sig: &Signature,
    indirect: bool,
    classify: &mut Classify<'_, E>,
) -> Result<(Vec<Axiom>, Vec<String>), Error<E>> {
    if sig.individuals.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut fresh: FxHashSet<Class> = FxHashSet::default();
    let mut of: Vec<(Class, &NamedIndividual)> = Vec::new();
    for (i, a) in sig.individuals.iter().enumerate() {
        let c = fresh_class(NOMINAL, i, &sig.classes);
        fresh.insert(c.clone());
        of.push((c, a));
    }
    let defs: Vec<AxiomKind> = of
        .iter()
        .map(|(c, a)| {
            AxiomKind::EquivalentClasses(
                [
                    ClassExpression::Class(c.clone()),
                    ClassExpression::ObjectOneOf([Individual::Named((*a).clone())].into()),
                ]
                .into(),
            )
        })
        .collect();
    let mut kinds = doc.logical_kinds();
    kinds.extend(defs.iter());
    let (classification, _) = classify(&kinds).map_err(Error::Classifier)?;
    drop(kinds);
    let view = View::new(&classification, &fresh);
    let mut out = Vec::new();
    for (c, a) in &of {
        let Some(n) = view.node_of(c) else { continue };
        let own: Vec<&Class> = view.members(n).collect();
        let mut nodes: Vec<usize> = Vec::new();
        if indirect {
            nodes.push(n);
            nodes.extend(view.strict_super_nodes(c));
        } else if !own.is_empty() {
            nodes.push(n);
        } else {
            nodes.extend(view.direct_super_nodes(c));
        }
        for m in nodes {
            for t in view.members(m) {
                out.push(Axiom::class_assertion(
                    t.clone(),
                    Individual::Named((*a).clone()),
                ));
            }
        }
    }
    Ok((out, Vec::new()))
}

/// Whether an axiom is an `OWLClassAxiom` of OWL API.
fn is_class_axiom(k: &AxiomKind) -> bool {
    matches!(
        k,
        AxiomKind::SubClassOf(..)
            | AxiomKind::EquivalentClasses(_)
            | AxiomKind::DisjointClasses(_)
            | AxiomKind::DisjointUnion(..)
    )
}

fn classes_of(k: &AxiomKind) -> Vec<Class> {
    let mut s = crate::model::Signature::default();
    s.axiom(k);
    s.0.into_iter()
        .filter_map(|e| match e {
            Entity::Class(c) => Some(c),
            _ => None,
        })
        .collect()
}

fn mentions_thing(k: &AxiomKind) -> bool {
    let mut s = crate::model::Signature::default();
    s.axiom(k);
    s.0.contains(&Entity::Class(thing()))
}

/// ROBOT's structural tautologies: `SubClassOf(X owl:Thing)`, `SubClassOf(owl:Nothing
/// X)`, `SubClassOf(X X)`, an equivalence of fewer than two classes, an assertion of
/// owl:Thing or of the top property, and the declaration of a built-in entity.
pub fn is_tautology(k: &AxiomKind) -> bool {
    match k {
        AxiomKind::SubClassOf(a, b) => {
            matches!(b, ClassExpression::Class(c) if c.as_str() == vocab::OWL_THING)
                || matches!(a, ClassExpression::Class(c) if c.as_str() == vocab::OWL_NOTHING)
                || a == b
        }
        AxiomKind::EquivalentClasses(cs) => cs.len() < 2,
        AxiomKind::ClassAssertion(ClassExpression::Class(c), _) => c.as_str() == vocab::OWL_THING,
        AxiomKind::ObjectPropertyAssertion(p, _, _) => {
            p.named().as_str() == vocab::OWL_TOP_OBJECT_PROPERTY
        }
        AxiomKind::DataPropertyAssertion(p, _, _) => p.as_str() == vocab::OWL_TOP_DATA_PROPERTY,
        AxiomKind::Declaration(e) => e.is_builtin(),
        _ => false,
    }
}
