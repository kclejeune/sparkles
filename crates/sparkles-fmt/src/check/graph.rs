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

/// The work [`isomorphic_bounded`] spends searching for a blank node mapping, counted in
/// quad lookups and candidate tries. Typical shapes graphs need a few hundred per blank
/// node, and ten thousand identical property shapes still fit.
pub const SEARCH_BUDGET: usize = 20_000_000;

/// Like [`isomorphic`], with the work bounded, and never wrong in the accepting direction.
/// `Some(true)` means a blank node mapping was found and checked quad by quad, and
/// `Some(false)` means the graphs differ. `None` means the budget ran out before either
/// was shown, and the caller must refuse. The fast path is the same as [`isomorphic`]'s.
/// When it misses, a graph with at most [`EXACT_BLANK_NODES`] blank nodes on each side is
/// canonicalized as before. A larger one is first compared by colour refinement, which
/// can only reject, and then a backtracking search guided by the colours looks for a
/// mapping. Canonicalizing many symmetric blank nodes takes factorial time and does not
/// check the formatter's deadline, so input from the network must not reach it.
pub fn isomorphic_bounded(a: &[Quad], b: &[Quad]) -> Option<bool> {
    isomorphic_within(a, b, SEARCH_BUDGET)
}

fn isomorphic_within(a: &[Quad], b: &[Quad], budget: usize) -> Option<bool> {
    if relabeled(a) == relabeled(b) {
        return Some(true);
    }
    if blank_nodes(a) <= EXACT_BLANK_NODES && blank_nodes(b) <= EXACT_BLANK_NODES {
        return Some(isomorphic(a, b));
    }
    // duplicates do not count
    let a: Vec<Quad> = a
        .iter()
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let b: Vec<Quad> = b
        .iter()
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if a.len() != b.len() {
        return Some(false);
    }
    let (ca, ha) = refined(&a);
    let (cb, hb) = refined(&b);
    if ha != hb {
        return Some(false);
    }
    search(&a, &ca, &b, &cb, budget)
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

/// The blank nodes of a term, subject or graph name, in order of occurrence.
fn blanks_of<'q>(q: &'q Quad, out: &mut Vec<&'q str>) {
    fn term<'q>(t: &'q Term, out: &mut Vec<&'q str>) {
        match t {
            Term::BlankNode(b) => out.push(b.as_str()),
            Term::Triple(t) => {
                if let NamedOrBlankNode::BlankNode(b) = &t.subject {
                    out.push(b.as_str());
                }
                term(&t.object, out);
            }
            _ => {}
        }
    }
    if let NamedOrBlankNode::BlankNode(b) = &q.subject {
        out.push(b.as_str());
    }
    term(&q.object, out);
    if let GraphName::BlankNode(b) = &q.graph_name {
        out.push(b.as_str());
    }
}

/// `q` with its blank nodes renamed through `map` (all of them must be in it).
fn mapped(q: &Quad, map: &HashMap<&str, &str>) -> Quad {
    fn blank(b: &BlankNode, map: &HashMap<&str, &str>) -> BlankNode {
        BlankNode::new_unchecked(map[b.as_str()])
    }
    fn subject(s: &NamedOrBlankNode, map: &HashMap<&str, &str>) -> NamedOrBlankNode {
        match s {
            NamedOrBlankNode::BlankNode(b) => blank(b, map).into(),
            other => other.clone(),
        }
    }
    fn term(t: &Term, map: &HashMap<&str, &str>) -> Term {
        match t {
            Term::BlankNode(b) => blank(b, map).into(),
            Term::Triple(t) => Triple::new(
                subject(&t.subject, map),
                t.predicate.clone(),
                term(&t.object, map),
            )
            .into(),
            other => other.clone(),
        }
    }
    Quad::new(
        subject(&q.subject, map),
        q.predicate.clone(),
        term(&q.object, map),
        match &q.graph_name {
            GraphName::BlankNode(b) => GraphName::BlankNode(blank(b, map)),
            other => other.clone(),
        },
    )
}

/// A backtracking search for a bijection of the blank nodes of `a` onto those of `b`
/// that maps every quad of `a` onto a quad of `b`. A blank node may only take a partner
/// of its own colour, and a quad is checked as soon as its blank nodes are all mapped.
/// `None` when `budget` steps were not enough.
fn search(
    a: &[Quad],
    ca: &HashMap<String, u64>,
    b: &[Quad],
    cb: &HashMap<String, u64>,
    budget: usize,
) -> Option<bool> {
    let b_set: HashSet<&Quad> = b.iter().collect();
    // the blank nodes of `a` in order of first occurrence, and the quads each is in
    let mut order: Vec<&str> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut quad_blanks: Vec<Vec<&str>> = Vec::with_capacity(a.len());
    for q in a {
        let mut v = Vec::new();
        blanks_of(q, &mut v);
        for x in &v {
            index.entry(*x).or_insert_with(|| {
                order.push(*x);
                order.len() - 1
            });
        }
        quad_blanks.push(v);
    }
    // a quad is checked when its last blank node (in `order`) is mapped
    let mut due: Vec<Vec<usize>> = vec![Vec::new(); order.len()];
    for (qi, v) in quad_blanks.iter().enumerate() {
        if let Some(last) = v.iter().map(|x| index[*x]).max() {
            due[last].push(qi);
        }
    }
    // a quad without blank nodes must be there as it is
    for (qi, v) in quad_blanks.iter().enumerate() {
        if v.is_empty() && !b_set.contains(&a[qi]) {
            return Some(false);
        }
    }
    let mut candidates: HashMap<u64, Vec<&str>> = HashMap::new();
    for k in cb.keys() {
        candidates.entry(cb[k]).or_default().push(k.as_str());
    }
    for v in candidates.values_mut() {
        v.sort_unstable();
    }
    if order.len() != cb.len() {
        return Some(false);
    }
    let mut map: HashMap<&str, &str> = HashMap::new();
    let mut used: HashSet<&str> = HashSet::new();
    // the next candidate to try for each depth
    let mut next: Vec<usize> = vec![0; order.len() + 1];
    let mut spent = 0usize;
    let mut depth = 0usize;
    loop {
        if depth == order.len() {
            return Some(true);
        }
        let x = order[depth];
        let Some(cands) = candidates.get(&ca[x]) else {
            return Some(false);
        };
        let mut placed = false;
        while next[depth] < cands.len() {
            let y = cands[next[depth]];
            next[depth] += 1;
            spent += 1;
            if used.contains(y) {
                continue;
            }
            map.insert(x, y);
            let mut ok = true;
            for &qi in &due[depth] {
                spent += 1;
                if !b_set.contains(&mapped(&a[qi], &map)) {
                    ok = false;
                    break;
                }
            }
            if spent > budget {
                return None;
            }
            if ok {
                used.insert(y);
                placed = true;
                break;
            }
            map.remove(x);
        }
        if placed {
            depth += 1;
            next[depth] = 0;
        } else if depth == 0 {
            return Some(false);
        } else {
            depth -= 1;
            let x = order[depth];
            let y = map.remove(x).expect("mapped");
            used.remove(y);
        }
    }
}

/// The colour of each blank node after refinement, and the sorted hashes of the quads
/// under those colours. Each round colours a blank node by its old colour and the sorted
/// descriptions of the quads it occurs in, with other blank nodes standing for their
/// colours. The rounds are fixed, so both sides of a comparison are refined alike.
fn refined(quads: &[Quad]) -> (HashMap<String, u64>, Vec<u64>) {
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
        let mut v = Vec::new();
        blanks_of(q, &mut v);
        for b in v {
            colour.insert(b.to_string(), 0);
        }
    }
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
        colour = colour
            .iter()
            .map(|(k, c)| {
                let mut v = seen.remove(k).unwrap_or_default();
                v.sort_unstable();
                (k.clone(), h(&(*c, v)))
            })
            .collect();
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
    (colour, out)
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

    /// Two nodes with `n` identical blank children each, listed in the given order.
    fn children(order: &[usize], n: usize) -> Vec<Quad> {
        let mut s = String::new();
        for &i in order {
            let parent = if i < n { "a" } else { "b" };
            s.push_str(&format!(
                "_:{parent} <http://e/p> _:c{i} . _:c{i} <http://e/q> 1 .\n"
            ));
        }
        quads(&s, Language::Turtle)
    }

    #[test]
    fn bounded_comparison_verifies_symmetric_blank_nodes() {
        let fwd: Vec<usize> = (0..22).collect();
        // not a mirror image, which numbers the blank nodes alike
        let rev: Vec<usize> = (0..22).step_by(2).chain((1..22).step_by(2)).collect();
        let start = std::time::Instant::now();
        assert_eq!(
            isomorphic_bounded(&children(&fwd, 11), &children(&rev, 11)),
            Some(true)
        );
        // one child fewer on one side
        assert_eq!(
            isomorphic_bounded(&children(&fwd, 11), &children(&fwd[1..], 11)),
            Some(false)
        );
        // the same shape with a different value on one child
        let mut other = children(&rev, 11);
        other.pop();
        other.push(Quad::new(
            oxrdf::BlankNode::new("zz").unwrap(),
            oxrdf::NamedNode::new("http://e/q").unwrap(),
            Literal::from(2),
            GraphName::DefaultGraph,
        ));
        assert_eq!(isomorphic_bounded(&children(&fwd, 11), &other), Some(false));
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    /// Two list-valued properties `p0` and `p1` of `shared` common entries, then the given
    /// last entries.
    fn lists(shared: usize, last: [&str; 2]) -> Vec<Quad> {
        let mut s = String::new();
        for (k, end) in last.iter().enumerate() {
            s.push_str(&format!("<http://e/s> <http://e/p{k}> _:l{k}_0 .\n"));
            for i in 0..shared {
                s.push_str(&format!(
                    "_:l{k}_{i} <http://e/first> <http://e/v{i}> . _:l{k}_{i} <http://e/rest> _:l{k}_{} .\n",
                    i + 1
                ));
            }
            s.push_str(&format!(
                "_:l{k}_{shared} <http://e/first> <http://e/{end}> . _:l{k}_{shared} <http://e/rest> <http://e/nil> .\n"
            ));
        }
        quads(&s, Language::Turtle)
    }

    #[test]
    fn graphs_that_refinement_cannot_tell_apart_are_not_accepted() {
        // two properties with long lists that share 30 entries and differ only in their
        // last: refinement cannot carry a tail back to its property, so exchanging the
        // tails gives another graph that refinement alone cannot tell from the first
        let a = lists(30, ["x", "y"]);
        let shuffled: Vec<Quad> = a.iter().rev().cloned().collect();
        assert_eq!(isomorphic_bounded(&a, &shuffled), Some(true));
        assert_eq!(isomorphic_bounded(&a, &lists(30, ["y", "x"])), Some(false));
        // ten identical blank nodes are not nine
        let ten = children(&(0..10).collect::<Vec<_>>(), 10);
        let nine = children(&(0..9).collect::<Vec<_>>(), 9);
        assert_eq!(isomorphic_bounded(&ten, &nine), Some(false));
        // and duplicates of one quad do not make a difference
        let mut dup = ten.clone();
        dup.extend(ten.clone());
        assert_eq!(isomorphic_bounded(&ten, &dup), Some(true));
    }

    #[test]
    fn an_exhausted_budget_is_not_an_answer() {
        let fwd: Vec<usize> = (0..22).collect();
        // not a mirror image, which numbers the blank nodes alike
        let rev: Vec<usize> = (0..22).step_by(2).chain((1..22).step_by(2)).collect();
        let (a, b) = (children(&fwd, 11), children(&rev, 11));
        assert_eq!(isomorphic_within(&a, &b, 5), None);
        assert_eq!(isomorphic_within(&a, &b, SEARCH_BUDGET), Some(true));
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
