//! SPIN rules as SHACL-AF SPARQL rules.
//!
//! A SPIN rule is a SPARQL CONSTRUCT attached to a class with `spin:rule`, or with a
//! subproperty of it, and SPIN runs it with `?this` bound to each instance of the class.
//! [`translate`] writes each one as a `sh:SPARQLRule` of a new node shape whose target is
//! that class, which is the same evaluation. A rule that does not mention `?this` runs
//! once in SPIN, so its shape targets the class node alone and the query runs once.
//!
//! The query is read from `sp:text`, which TopBraid writes beside the RDF form of a SPIN
//! query. A query given only in SPIN's RDF syntax, and a call of a SPIN template, are
//! refused with an error that names the rule. SPIN's rule properties all run in one
//! group, which iterates to a fixed point as TopBraid's SPIN inferencing does.

use crate::shapes::G;
use crate::vocab::{rdf, sh};
use anyhow::{Result, bail};
use oxrdf::{BlankNode, Graph, Literal, NamedNode, NamedNodeRef, NamedOrBlankNode, Term, Triple};
use rustc_hash::FxHashSet;

/// The SPIN namespace.
pub const SPIN_NS: &str = "http://spinrdf.org/spin#";
/// The SPIN SPARQL syntax namespace.
pub const SP_NS: &str = "http://spinrdf.org/sp#";

const SPIN_RULE: NamedNodeRef<'static> =
    NamedNodeRef::new_unchecked("http://spinrdf.org/spin#rule");
const SP_TEXT: NamedNodeRef<'static> = NamedNodeRef::new_unchecked("http://spinrdf.org/sp#text");

/// The prefixes a SPIN query may use without declaring them: those of RDF, RDFS, OWL,
/// XSD, SHACL and SPIN's own vocabularies.
pub const STANDARD_PREFIXES: &[(&str, &str)] = &[
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
    ("sh", "http://www.w3.org/ns/shacl#"),
    ("sp", SP_NS),
    ("spin", SPIN_NS),
    ("spl", "http://spinrdf.org/spl#"),
];

/// Whether a graph holds SPIN rules.
pub fn has_rules(graph: &Graph) -> bool {
    !rule_properties(graph).is_empty()
}

/// `spin:rule` and its subproperties (`rdfs:subPropertyOf*`).
fn rule_properties(graph: &Graph) -> Vec<NamedNode> {
    let mut out = vec![SPIN_RULE.into_owned()];
    let mut i = 0;
    while i < out.len() {
        let p = Term::NamedNode(out[i].clone());
        for t in graph.subjects_for_predicate_object(oxrdf::vocab::rdfs::SUB_PROPERTY_OF, &p) {
            if let NamedOrBlankNode::NamedNode(n) = t.into_owned()
                && !out.contains(&n)
            {
                out.push(n);
            }
        }
        i += 1;
    }
    out.retain(|p| graph.triples_for_predicate(p).next().is_some());
    out
}

/// The SHACL-AF triples of the SPIN rules of `graph`: for each class with rules, a node
/// shape with the class as its target and one `sh:SPARQLRule` per rule. The queries get
/// the declarations of `prefixes` and of [`STANDARD_PREFIXES`] before their own text.
pub fn translate(graph: &Graph, prefixes: &[(String, String)]) -> Result<Graph> {
    let g = G { g: graph };
    let mut head = String::new();
    let mut declared = FxHashSet::default();
    for (p, ns) in prefixes
        .iter()
        .map(|(p, n)| (p.as_str(), n.as_str()))
        .chain(STANDARD_PREFIXES.iter().copied())
    {
        if declared.insert(p) {
            head.push_str(&format!("PREFIX {p}: <{ns}>\n"));
        }
    }
    let mut pairs: Vec<(Term, Term)> = Vec::new();
    for p in rule_properties(graph) {
        for t in graph.triples_for_predicate(&p) {
            pairs.push((Term::from(t.subject.into_owned()), t.object.into_owned()));
        }
    }
    pairs.sort_by_cached_key(|(c, r)| (c.to_string(), r.to_string()));
    pairs.dedup();
    let mut out = Graph::new();
    let mut n = 0usize;
    let mut fresh = || {
        n += 1;
        BlankNode::new_unchecked(format!("spinr{n}"))
    };
    for (class, rule) in pairs {
        let Term::NamedNode(class) = class else {
            bail!("the SPIN rule {rule} belongs to {class}, which is not an IRI");
        };
        let text = match g.object(&rule, SP_TEXT) {
            Some(Term::Literal(l)) => l.value().to_string(),
            Some(t) => bail!("the sp:text of the SPIN rule {rule} is {t}, not a string"),
            None => {
                let kind = g
                    .object(&rule, rdf::TYPE)
                    .map_or_else(|| "no type".to_string(), |t| t.to_string());
                bail!(
                    "the SPIN rule {rule} of {class} ({kind}) has no sp:text: only rules with \
                     their query text are translated, not SPIN's RDF syntax or templates"
                );
            }
        };
        let shape = fresh();
        let r = fresh();
        let mentions_this = text.contains("?this") || text.contains("$this");
        let target = if mentions_this {
            sh::TARGET_CLASS
        } else {
            sh::TARGET_NODE
        };
        out.insert(&Triple::new(shape.clone(), rdf::TYPE, sh::NODE_SHAPE));
        out.insert(&Triple::new(shape.clone(), target, class.clone()));
        out.insert(&Triple::new(shape.clone(), sh::RULE, r.clone()));
        out.insert(&Triple::new(r.clone(), rdf::TYPE, sh::SPARQL_RULE));
        out.insert(&Triple::new(
            r,
            sh::CONSTRUCT,
            Literal::new_simple_literal(format!("{head}{text}")),
        ));
    }
    Ok(out)
}
