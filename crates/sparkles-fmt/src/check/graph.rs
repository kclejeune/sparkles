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
        Language::Sparql | Language::JsonLd | Language::Shaclc => {
            Err(FormatError::unsupported_language(lang))
        }
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

/// The most blank nodes [`isomorphic_bounded`] canonicalizes exactly.
pub const EXACT_BLANK_NODES: usize = 8;

/// Like [`isomorphic`], with the work bounded. The fast path is the same. When it fails,
/// a graph with at most [`EXACT_BLANK_NODES`] blank nodes on each side is canonicalized
/// as before. A larger one is compared by colour refinement, which is polynomial but
/// weaker. It can call two non-isomorphic graphs equal only when refinement cannot tell
/// their blank nodes apart, which no graph a formatter's reordering produces does.
/// Canonicalizing many symmetric blank nodes takes factorial time and does not check
/// the formatter's deadline, so input from the network must not reach it.
pub fn isomorphic_bounded(a: &[Quad], b: &[Quad]) -> bool {
    if relabeled(a) == relabeled(b) {
        return true;
    }
    if blank_nodes(a) <= EXACT_BLANK_NODES && blank_nodes(b) <= EXACT_BLANK_NODES {
        return isomorphic(a, b);
    }
    refined(a) == refined(b)
}

fn blank_nodes(quads: &[Quad]) -> usize {
    let mut names: HashMap<&str, usize> = HashMap::new();
    for q in quads {
        let mut r = Relabel(&mut names);
        r.subject(&q.subject);
        r.term(&q.object);
        if let GraphName::BlankNode(b) = &q.graph_name {
            r.blank(b);
        }
    }
    names.len()
}

/// The sorted hashes of the quads after colour refinement of the blank nodes: each
/// round colours a blank node by its old colour and the sorted descriptions of the quads
/// it occurs in, with other blank nodes standing for their colours.
fn refined(quads: &[Quad]) -> Vec<u64> {
    use std::hash::{DefaultHasher, Hash, Hasher};
    fn h<T: Hash>(t: &T) -> u64 {
        let mut s = DefaultHasher::new();
        t.hash(&mut s);
        s.finish()
    }
    // a term as a number: a blank node by its colour, anything else by its text
    fn term(t: &Term, colour: &HashMap<String, u64>) -> u64 {
        match t {
            Term::BlankNode(b) => h(&(1u8, colour[b.as_str()])),
            other => h(&(2u8, other.to_string())),
        }
    }
    fn subject(s: &NamedOrBlankNode, colour: &HashMap<String, u64>) -> u64 {
        match s {
            NamedOrBlankNode::BlankNode(b) => h(&(1u8, colour[b.as_str()])),
            other => h(&(2u8, other.to_string())),
        }
    }
    fn graph(g: &GraphName, colour: &HashMap<String, u64>) -> u64 {
        match g {
            GraphName::BlankNode(b) => h(&(1u8, colour[b.as_str()])),
            other => h(&(2u8, other.to_string())),
        }
    }
    let mut colour: HashMap<String, u64> = HashMap::new();
    for q in quads {
        if let NamedOrBlankNode::BlankNode(b) = &q.subject {
            colour.insert(b.as_str().to_string(), 0);
        }
        if let Term::BlankNode(b) = &q.object {
            colour.insert(b.as_str().to_string(), 0);
        }
        if let GraphName::BlankNode(b) = &q.graph_name {
            colour.insert(b.as_str().to_string(), 0);
        }
    }
    // the rounds are fixed, so both sides of a comparison are refined alike
    for _ in 0..12 {
        let mut seen: HashMap<String, Vec<u64>> = HashMap::new();
        for q in quads {
            let (s, o, g) = (
                subject(&q.subject, &colour),
                term(&q.object, &colour),
                graph(&q.graph_name, &colour),
            );
            let p = h(&q.predicate.as_str());
            if let NamedOrBlankNode::BlankNode(b) = &q.subject {
                seen.entry(b.as_str().to_string())
                    .or_default()
                    .push(h(&(0u8, p, o, g)));
            }
            if let Term::BlankNode(b) = &q.object {
                seen.entry(b.as_str().to_string())
                    .or_default()
                    .push(h(&(1u8, p, s, g)));
            }
            if let GraphName::BlankNode(b) = &q.graph_name {
                seen.entry(b.as_str().to_string())
                    .or_default()
                    .push(h(&(2u8, p, s, o)));
            }
        }
        let next: HashMap<String, u64> = colour
            .iter()
            .map(|(k, c)| {
                let mut v = seen.remove(k).unwrap_or_default();
                v.sort_unstable();
                (k.clone(), h(&(*c, v)))
            })
            .collect();
        colour = next;
    }
    let mut out: Vec<u64> = quads
        .iter()
        .map(|q| {
            h(&(
                subject(&q.subject, &colour),
                q.predicate.as_str(),
                term(&q.object, &colour),
                graph(&q.graph_name, &colour),
            ))
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
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
    fn bounded_comparison_does_not_blow_up_on_symmetric_blank_nodes() {
        let t = Language::Turtle;
        // two nodes with eleven identical blank children each, listed in different orders
        let doc = |order: &[usize]| {
            let mut s = String::new();
            for &i in order {
                let parent = if i < 11 { "a" } else { "b" };
                s.push_str(&format!(
                    "_:{parent} <http://e/p> _:c{i} . _:c{i} <http://e/q> 1 .\n"
                ));
            }
            quads(&s, t)
        };
        let fwd: Vec<usize> = (0..22).collect();
        let rev: Vec<usize> = (0..22).rev().collect();
        let start = std::time::Instant::now();
        assert!(isomorphic_bounded(&doc(&fwd), &doc(&rev)));
        // one child fewer on one side
        assert!(!isomorphic_bounded(&doc(&fwd), &doc(&fwd[1..])));
        // the same shape with a different value on one child
        let mut other = doc(&rev);
        other.pop();
        other.push(Quad::new(
            oxrdf::BlankNode::new("zz").unwrap(),
            oxrdf::NamedNode::new("http://e/q").unwrap(),
            Literal::from(2),
            GraphName::DefaultGraph,
        ));
        assert!(!isomorphic_bounded(&doc(&fwd), &other));
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
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
