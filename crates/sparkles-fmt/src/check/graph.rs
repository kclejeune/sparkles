//! The graph check of the RDF syntaxes (Turtle, TriG, N-Triples, N-Quads): the reference
//! parse with oxttl, whose errors are the user's syntax errors, and the comparison of the
//! input's and the output's quads as datasets (isomorphic: equal up to blank node
//! labels).

use crate::{Check, FormatError, Language};
use oxrdf::dataset::CanonicalizationAlgorithm;
use oxrdf::{BlankNode, Dataset, GraphName, Literal, NamedOrBlankNode, Quad, Term, Triple};
use oxttl::{NQuadsParser, NTriplesParser, TriGParser, TurtleParser};
use std::collections::{HashMap, HashSet};

/// The base IRI of both parses, so relative IRIs resolve (a `BASE` in the text wins).
pub const RDF_BASE: &str = super::SPARQL_BASE;

/// The reference parse of an RDF document.
#[derive(Clone, Debug)]
pub struct RdfReference {
    pub language: Language,
    /// the quads in document order (triples in the default graph), blank nodes as oxttl
    /// made them: labels kept, `[]` and the like fresh
    pub quads: Vec<Quad>,
}

/// Parse `text` (a BOM dropped) as `lang`, one of the four RDF syntaxes, with oxttl and
/// the synthetic base.
pub fn rdf_reference(text: &str, lang: Language) -> Result<RdfReference, FormatError> {
    Ok(RdfReference {
        language: lang,
        quads: parse(text, lang)?,
    })
}

/// `Ok` when `output` parses (as the input's language) to a dataset isomorphic to the
/// input's.
pub fn rdf_equivalent(r: &RdfReference, output: &str) -> Result<(), FormatError> {
    let unsafe_ = FormatError::Unsafe {
        check: Check::Graph,
    };
    let out = parse(output, r.language).map_err(|_| unsafe_.clone())?;
    if isomorphic(&r.quads, &out) {
        Ok(())
    } else {
        Err(unsafe_)
    }
}

/// The quads of `text` as `lang`.
pub fn parse(text: &str, lang: Language) -> Result<Vec<Quad>, FormatError> {
    let bom = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let body = &text[bom..];
    let positioned = |e: oxttl::TurtleSyntaxError| {
        let offset = bom + e.location().start.offset as usize;
        let (line, column) = crate::line_col(text, offset);
        FormatError::Syntax {
            message: e.message().to_string(),
            line,
            column,
            offset,
        }
    };
    let triples = |it: &mut dyn Iterator<Item = Result<Triple, oxttl::TurtleSyntaxError>>| {
        it.map(|t| t.map(|t| t.in_graph(GraphName::DefaultGraph)))
            .collect::<Result<Vec<Quad>, _>>()
            .map_err(positioned)
    };
    match lang {
        Language::Turtle => triples(
            &mut TurtleParser::new()
                .with_base_iri(RDF_BASE)
                .expect("a valid base IRI")
                .for_slice(body),
        ),
        Language::TriG => TriGParser::new()
            .with_base_iri(RDF_BASE)
            .expect("a valid base IRI")
            .for_slice(body)
            .collect::<Result<Vec<Quad>, _>>()
            .map_err(positioned),
        Language::NTriples => triples(&mut NTriplesParser::new().for_slice(body)),
        Language::NQuads => NQuadsParser::new()
            .for_slice(body)
            .collect::<Result<Vec<Quad>, _>>()
            .map_err(positioned),
        Language::Sparql | Language::JsonLd => Err(FormatError::unsupported_language(lang)),
    }
}

/// Whether `a` and `b` denote isomorphic datasets (duplicates do not count). The fast
/// path relabels blank nodes in order of first occurrence and compares the sets of quads;
/// when a reordering moved first occurrences, both sides are canonicalized.
pub fn isomorphic(a: &[Quad], b: &[Quad]) -> bool {
    if relabeled(a) == relabeled(b) {
        return true;
    }
    let canonical = |quads: &[Quad]| {
        let mut d: Dataset = quads.iter().cloned().collect();
        d.canonicalize(CanonicalizationAlgorithm::Unstable);
        d
    };
    canonical(a) == canonical(b)
}

/// A term with its blank node numbered by first occurrence, borrowing the rest: the fast
/// path compares these without copying any string.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key<'a> {
    Named(&'a str),
    Blank(usize),
    Literal(&'a Literal),
    Triple(Box<(Key<'a>, &'a str, Key<'a>)>),
    DefaultGraph,
}

/// The set of the quads with blank nodes numbered in order of first occurrence.
fn relabeled(quads: &[Quad]) -> HashSet<(Key<'_>, &str, Key<'_>, Key<'_>)> {
    let mut names: HashMap<&str, usize> = HashMap::new();
    let mut out = HashSet::with_capacity(quads.len());
    for q in quads {
        let mut r = Relabel(&mut names);
        let quad = (
            r.subject(&q.subject),
            q.predicate.as_str(),
            r.term(&q.object),
            match &q.graph_name {
                GraphName::NamedNode(n) => Key::Named(n.as_str()),
                GraphName::BlankNode(b) => r.blank(b),
                GraphName::DefaultGraph => Key::DefaultGraph,
            },
        );
        out.insert(quad);
    }
    out
}

struct Relabel<'m, 'a>(&'m mut HashMap<&'a str, usize>);

impl<'a> Relabel<'_, 'a> {
    fn blank(&mut self, b: &'a BlankNode) -> Key<'a> {
        let n = self.0.len();
        Key::Blank(*self.0.entry(b.as_str()).or_insert(n))
    }

    fn subject(&mut self, s: &'a NamedOrBlankNode) -> Key<'a> {
        match s {
            NamedOrBlankNode::NamedNode(n) => Key::Named(n.as_str()),
            NamedOrBlankNode::BlankNode(b) => self.blank(b),
        }
    }

    fn term(&mut self, t: &'a Term) -> Key<'a> {
        match t {
            Term::NamedNode(n) => Key::Named(n.as_str()),
            Term::BlankNode(b) => self.blank(b),
            Term::Literal(l) => Key::Literal(l),
            Term::Triple(t) => Key::Triple(Box::new((
                self.subject(&t.subject),
                t.predicate.as_str(),
                self.term(&t.object),
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quads(text: &str, lang: Language) -> Vec<Quad> {
        parse(text, lang).unwrap()
    }

    #[test]
    fn isomorphic_up_to_labels_and_order() {
        let t = Language::Turtle;
        let a = quads("_:x <http://e/p> _:y . _:y <http://e/q> 1 .", t);
        let b = quads("_:m <http://e/q> 1 . _:n <http://e/p> _:m .", t);
        assert!(isomorphic(&a, &b));
        let c = quads("_:m <http://e/q> 1 . _:n <http://e/p> _:n .", t);
        assert!(!isomorphic(&a, &c));
        // duplicates do not count; anonymous nodes are fresh in each parse
        let d = quads("[] <http://e/p> 1 . [] <http://e/p> 1 .", t);
        let e = quads(
            "_:a <http://e/p> 1 . _:b <http://e/p> 1 . _:b <http://e/p> 1 .",
            t,
        );
        assert!(isomorphic(&d, &e));
        assert!(!isomorphic(&d, &quads("_:a <http://e/p> 1 .", t)));
    }

    #[test]
    fn reference_errors_are_positioned() {
        let e = rdf_reference("<a> <b> <c> .\n<a> <b> .", Language::Turtle).unwrap_err();
        assert!(matches!(e, FormatError::Syntax { line: 2, .. }), "{e:?}");
        let e = rdf_reference("\u{feff}<http://a> <http://b> .", Language::NTriples).unwrap_err();
        let FormatError::Syntax { line, offset, .. } = e else {
            panic!("{e:?}")
        };
        assert_eq!(line, 1);
        assert!(offset >= 3);
        // relative IRIs resolve against the synthetic base
        assert!(rdf_reference("<a> <b> <c> .", Language::Turtle).is_ok());
        assert!(rdf_reference("GRAPH <g> { <a> <b> <c> }", Language::TriG).is_ok());
    }

    #[test]
    fn graph_differs() {
        let r = rdf_reference("<a> <b> <c> .", Language::Turtle).unwrap();
        assert!(rdf_equivalent(&r, "<a> <b> <c> .\n").is_ok());
        assert_eq!(
            rdf_equivalent(&r, "<a> <b> <d> ."),
            Err(FormatError::Unsafe {
                check: Check::Graph
            })
        );
        assert!(rdf_equivalent(&r, "<a> <b> .").is_err());
    }
}
