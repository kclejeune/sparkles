//! The schema report as RDF: a VoID description of the observed statistics, following
//! the W3C VoID Interest Group Note, optionally followed by the declared RDFS/OWL schema
//! as asserted triples.
//!
//! The description is one `void:Dataset` node, `urn:x-sparkles:schema:<dataset>:<version>`,
//! with the totals of the selection, one `void:classPartition` per class that has
//! instances and one `void:propertyPartition` per predicate that is used. Partitions are
//! blank nodes. Every number is an exact count at the report's snapshot.

use super::{Lit, SchemaReport};
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{BlankNode, Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use oxrdfio::{RdfFormat, RdfSerializer};

/// The VoID namespace.
pub const VOID_NS: &str = "http://rdfs.org/ns/void#";
const DCTERMS: &str = "http://purl.org/dc/terms/";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// What [`void_triples`] writes.
#[derive(Clone, Debug, Default)]
pub struct VoidOptions<'a> {
    /// The dataset name, used in the description's IRI and as its `dcterms:title`.
    pub dataset: &'a str,
    /// Also write the declarations (types, `rdfs:subClassOf`, domains, ranges, labels and
    /// so on) of the report's classes, predicates and ontologies.
    pub declarations: bool,
    /// Prefixes for the syntaxes that use them, besides `void`, `dcterms`, `rdf`,
    /// `rdfs`, `owl` and `xsd`.
    pub prefixes: Vec<(String, String)>,
}

/// The IRI of the description of `dataset` at snapshot `version`:
/// `urn:x-sparkles:schema:<dataset>:<version>`, with the name percent-encoded.
pub fn description_iri(dataset: &str, version: u64) -> String {
    let mut name = String::with_capacity(dataset.len());
    for b in dataset.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            name.push(b as char);
        } else {
            name.push_str(&format!("%{b:02X}"));
        }
    }
    format!("urn:x-sparkles:schema:{name}:{version}")
}

fn iri(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}

fn void(local: &str) -> NamedNode {
    iri(&format!("{VOID_NS}{local}"))
}

fn count(n: u64) -> Term {
    Literal::new_typed_literal(n.to_string(), xsd::INTEGER).into()
}

fn lit(l: &Lit) -> Term {
    match &l.lang {
        Some(lang) => Literal::new_language_tagged_literal(&l.value, lang)
            .unwrap_or_else(|_| Literal::new_simple_literal(&l.value)),
        None => Literal::new_simple_literal(&l.value),
    }
    .into()
}

/// Triples of one subject, in the order they are added.
struct Out(Vec<Triple>);

impl Out {
    fn add(&mut self, s: &NamedOrBlankNode, p: NamedNode, o: impl Into<Term>) {
        self.0.push(Triple::new(s.clone(), p, o));
    }

    fn iris<'a>(
        &mut self,
        s: &NamedOrBlankNode,
        p: &str,
        objects: impl IntoIterator<Item = &'a String>,
    ) {
        for o in objects {
            if let Ok(o) = NamedNode::new(o.as_str()) {
                self.add(s, iri(p), o);
            }
        }
    }

    fn lits<'a>(
        &mut self,
        s: &NamedOrBlankNode,
        p: &str,
        objects: impl IntoIterator<Item = &'a Lit>,
    ) {
        for o in objects {
            self.add(s, iri(p), lit(o));
        }
    }
}

/// The VoID description of `report` (and its declarations, if asked for).
pub fn void_triples(report: &SchemaReport, opts: &VoidOptions) -> Vec<Triple> {
    let mut out = Out(Vec::new());
    let d: NamedOrBlankNode = iri(&description_iri(opts.dataset, report.snapshot.version)).into();
    let classes: Vec<_> = report
        .classes
        .iter()
        .filter(|c| c.observed.instances > 0)
        .collect();
    let properties: Vec<_> = report
        .predicates
        .iter()
        .filter(|p| p.observed.triples > 0)
        .collect();
    out.add(&d, rdf::TYPE.into_owned(), void("Dataset"));
    out.add(
        &d,
        iri(&format!("{DCTERMS}title")),
        Literal::new_simple_literal(opts.dataset),
    );
    out.add(
        &d,
        iri(&format!("{DCTERMS}created")),
        Literal::new_typed_literal(&report.snapshot.computed_at, xsd::DATE_TIME),
    );
    out.add(&d, void("triples"), count(report.totals.triples));
    if let Some(t) = report.term_totals {
        out.add(&d, void("entities"), count(t.entities));
    }
    // VoID counts the distinct objects of rdf:type, blank nodes included
    let n_classes = classes.len() as u64 + report.totals.anonymous_type_targets;
    out.add(&d, void("classes"), count(n_classes));
    out.add(&d, void("properties"), count(properties.len() as u64));
    if let Some(t) = report.term_totals {
        out.add(&d, void("distinctSubjects"), count(t.distinct_subjects));
        out.add(&d, void("distinctObjects"), count(t.distinct_objects));
    }
    let blank = |prefix: &str, i: usize| -> NamedOrBlankNode {
        BlankNode::new_unchecked(format!("{prefix}{}", i + 1)).into()
    };
    for i in 0..classes.len() {
        out.add(&d, void("classPartition"), blank("c", i));
    }
    for i in 0..properties.len() {
        out.add(&d, void("propertyPartition"), blank("p", i));
    }
    for (i, c) in classes.iter().enumerate() {
        let b = blank("c", i);
        out.add(&b, void("class"), iri(&c.iri));
        out.add(&b, void("entities"), count(c.observed.instances));
    }
    for (i, p) in properties.iter().enumerate() {
        let b = blank("p", i);
        let o = &p.observed;
        out.add(&b, void("property"), iri(&p.iri));
        out.add(&b, void("triples"), count(o.triples));
        out.add(&b, void("distinctSubjects"), count(o.distinct_subjects));
        out.add(&b, void("distinctObjects"), count(o.distinct_objects));
    }
    if opts.declarations {
        declarations(report, &mut out);
    }
    out.0
}

/// The declared layer as the triples that assert it.
fn declarations(report: &SchemaReport, out: &mut Out) {
    let rdf_type = rdf::TYPE.as_str();
    let rdfs = |l: &str| format!("{RDFS}{l}");
    let owl = |l: &str| format!("{OWL}{l}");
    for o in &report.ontology {
        let s: NamedOrBlankNode = iri(&o.iri).into();
        out.add(&s, rdf::TYPE.into_owned(), iri(&owl("Ontology")));
        out.lits(&s, &rdfs("label"), &o.labels);
        out.lits(&s, &owl("versionInfo"), &o.version_info);
        out.lits(&s, &rdfs("comment"), &o.comments);
    }
    for c in &report.classes {
        let d = &c.declared;
        let s: NamedOrBlankNode = iri(&c.iri).into();
        out.iris(&s, rdf_type, &d.types);
        out.iris(&s, &rdfs("subClassOf"), &d.super_classes);
        out.iris(&s, &owl("equivalentClass"), &d.equivalent_classes);
        out.iris(&s, &owl("disjointWith"), &d.disjoint_with);
        out.lits(&s, &rdfs("label"), &d.labels);
        out.lits(&s, &rdfs("comment"), &d.comments);
    }
    for p in &report.predicates {
        let d = &p.declared;
        let s: NamedOrBlankNode = iri(&p.iri).into();
        out.iris(&s, rdf_type, &d.types);
        out.iris(&s, &rdfs("domain"), &d.domains);
        out.iris(&s, &rdfs("range"), &d.ranges);
        out.iris(&s, &rdfs("subPropertyOf"), &d.super_properties);
        out.iris(&s, &owl("inverseOf"), &d.inverse_of);
        out.lits(&s, &rdfs("label"), &d.labels);
        out.lits(&s, &rdfs("comment"), &d.comments);
    }
}

/// [`void_triples`] in an RDF syntax (a dataset syntax puts them in the default graph).
pub fn void_text(report: &SchemaReport, opts: &VoidOptions, format: RdfFormat) -> String {
    let mut prefixes: Vec<(String, String)> = [
        ("void", VOID_NS),
        ("dcterms", DCTERMS),
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("rdfs", RDFS),
        ("owl", OWL),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ]
    .into_iter()
    .map(|(p, ns)| (p.to_string(), ns.to_string()))
    .collect();
    for (p, ns) in &opts.prefixes {
        if !prefixes.iter().any(|(q, _)| q == p) {
            prefixes.push((p.clone(), ns.clone()));
        }
    }
    let triples = void_triples(report, opts);
    // only the prefixes the description uses
    let used = |ns: &str| {
        triples.iter().any(|t| {
            let named = |x: &Term| matches!(x, Term::NamedNode(n) if n.as_str().starts_with(ns));
            t.predicate.as_str().starts_with(ns)
                || matches!(&t.subject, NamedOrBlankNode::NamedNode(n) if n.as_str().starts_with(ns))
                || named(&t.object)
                || matches!(&t.object, Term::Literal(l) if l.datatype().as_str().starts_with(ns))
        })
    };
    prefixes.retain(|(_, ns)| used(ns));
    let ser = crate::io::with_prefixes(RdfSerializer::from_format(format), prefixes);
    let mut w = ser.for_writer(Vec::new());
    for t in triples {
        w.serialize_triple(&t)
            .expect("writing to memory does not fail");
    }
    String::from_utf8(w.finish().expect("writing to memory does not fail"))
        .expect("RDF syntaxes are UTF-8")
}
