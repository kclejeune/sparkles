//! The graph check of the RDF syntaxes (Turtle, TriG, N-Triples, N-Quads): the reference
//! parse with oxttl, whose errors are the user's syntax errors, and the comparison of the
//! input's and the output's quads as datasets (isomorphic: equal up to blank node
//! labels).

use crate::{Check, FormatError, Language};
use oxrdf::dataset::CanonicalizationAlgorithm;
use oxrdf::{
    BlankNode, Dataset, GraphName, Literal, NamedOrBlankNode, Quad, Term, TermRef, Triple,
};
use oxttl::{NQuadsParser, NTriplesParser, TriGParser, TurtleParser};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

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
/// candidate tries and quad lookups. Typical shapes graphs need a few hundred per blank
/// node, and ten thousand identical property shapes still fit.
pub const SEARCH_BUDGET: usize = 20_000_000;

/// Like [`isomorphic`], with the work bounded, and never wrong in the accepting direction.
/// `Ok(Some(true))` means a blank node mapping was found and checked quad by quad, and
/// `Ok(Some(false))` means the graphs differ. `Ok(None)` means the budget ran out before
/// either was shown, and the caller must refuse. `Err(Timeout)` means `deadline` passed.
/// The fast path is the same as [`isomorphic`]'s. When it misses, a graph with at most
/// [`EXACT_BLANK_NODES`] blank nodes on each side is canonicalized as before. A larger
/// one is first compared by colour refinement, which can only reject, and then a
/// backtracking search guided by the colours looks for a mapping. The terms are interned
/// to small numbers once, so no step hashes or copies a term. Canonicalizing many
/// symmetric blank nodes takes factorial time and does not check the deadline, so input
/// from the network must not reach it.
pub fn isomorphic_bounded(
    a: &[Quad],
    b: &[Quad],
    deadline: Option<Instant>,
) -> Result<Option<bool>, FormatError> {
    isomorphic_within(a, b, SEARCH_BUDGET, deadline)
}

fn isomorphic_within(
    a: &[Quad],
    b: &[Quad],
    budget: usize,
    deadline: Option<Instant>,
) -> Result<Option<bool>, FormatError> {
    if relabeled(a) == relabeled(b) {
        return Ok(Some(true));
    }
    deadline_check(deadline)?;
    if blank_nodes(a) <= EXACT_BLANK_NODES && blank_nodes(b) <= EXACT_BLANK_NODES {
        return Ok(Some(isomorphic(a, b)));
    }
    let Some(mut g) = Interned::new(a, b) else {
        return Ok(None);
    };
    g.a.quads.sort_unstable();
    g.a.quads.dedup();
    g.b.quads.sort_unstable();
    g.b.quads.dedup();
    if g.a.quads.len() != g.b.quads.len() {
        return Ok(Some(false));
    }
    let ca = g.a.refined(deadline)?;
    let cb = g.b.refined(deadline)?;
    if ca.1 != cb.1 {
        return Ok(Some(false));
    }
    search(&g.a, &ca.0, &g.b, &cb.0, budget, deadline)
}

fn deadline_check(d: Option<Instant>) -> Result<(), FormatError> {
    super::deadline(d)
}

/// A countdown that looks at the clock every so many steps.
struct Ticker {
    deadline: Option<Instant>,
    left: u32,
}

impl Ticker {
    const EVERY: u32 = 1024;

    fn new(deadline: Option<Instant>) -> Ticker {
        Ticker {
            deadline,
            left: Self::EVERY,
        }
    }

    fn tick(&mut self) -> Result<(), FormatError> {
        self.left -= 1;
        if self.left == 0 {
            self.left = Self::EVERY;
            deadline_check(self.deadline)?;
        }
        Ok(())
    }
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

/// A term of an interned quad: a constant's number, or a blank node's number with
/// [`BLANK`] set.
type Code = u32;
const BLANK: Code = 1 << 31;

/// One side of a comparison: quads of `[subject, predicate, object, graph]` codes.
struct Side {
    quads: Vec<[Code; 4]>,
    /// how many blank nodes, numbered in order of first occurrence
    blanks: usize,
}

/// Both sides, with the constants numbered together so equal terms have equal codes.
struct Interned {
    a: Side,
    b: Side,
}

fn has_blank(t: &Triple) -> bool {
    fn term(t: &Term) -> bool {
        match t {
            Term::BlankNode(_) => true,
            Term::Triple(t) => has_blank(t),
            _ => false,
        }
    }
    matches!(t.subject, NamedOrBlankNode::BlankNode(_)) || term(&t.object)
}

impl Interned {
    /// `None` when a quad has a blank node inside a triple term, which the search does
    /// not map.
    fn new(a: &[Quad], b: &[Quad]) -> Option<Interned> {
        // the key borrows from the quads
        fn side<'q>(quads: &'q [Quad], constants: &mut HashMap<TermRef<'q>, Code>) -> Option<Side> {
            let mut blanks: HashMap<&'q str, Code> = HashMap::new();
            let mut code = |t: TermRef<'q>| -> Option<Code> {
                Some(match t {
                    TermRef::BlankNode(b) => {
                        let n = blanks.len() as Code;
                        BLANK | *blanks.entry(b.as_str()).or_insert(n)
                    }
                    TermRef::Triple(t) if has_blank(t) => return None,
                    t => {
                        // 0 is the default graph
                        let n = constants.len() as Code + 1;
                        *constants.entry(t).or_insert(n)
                    }
                })
            };
            let mut out = Vec::with_capacity(quads.len());
            for q in quads {
                let graph = match &q.graph_name {
                    GraphName::DefaultGraph => 0,
                    GraphName::NamedNode(n) => code(TermRef::NamedNode(n.as_ref()))?,
                    GraphName::BlankNode(b) => code(TermRef::BlankNode(b.as_ref()))?,
                };
                out.push([
                    code(q.subject.as_ref().into())?,
                    code(TermRef::NamedNode(q.predicate.as_ref()))?,
                    code(q.object.as_ref())?,
                    graph,
                ]);
            }
            let blanks = blanks.len();
            Some(Side { quads: out, blanks })
        }
        let mut constants = HashMap::new();
        let a = side(a, &mut constants)?;
        let b = side(b, &mut constants)?;
        Some(Interned { a, b })
    }
}

fn mix<T: std::hash::Hash>(t: &T) -> u64 {
    use std::hash::Hasher;
    let mut s = std::hash::DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
}

impl Side {
    /// The colour of each blank node after refinement, and the sorted hashes of the
    /// quads under those colours. Each round colours a blank node by its old colour and
    /// the sorted descriptions of the quads it occurs in, with other blank nodes standing
    /// for their colours. The rounds are fixed, so both sides of a comparison are
    /// refined alike.
    fn refined(&self, deadline: Option<Instant>) -> Result<(Vec<u64>, Vec<u64>), FormatError> {
        let mut tick = Ticker::new(deadline);
        let mut colour = vec![0u64; self.blanks];
        let of = |c: Code, colour: &[u64]| -> u64 {
            if c & BLANK != 0 {
                mix(&(1u8, colour[(c & !BLANK) as usize]))
            } else {
                mix(&(2u8, c))
            }
        };
        for _ in 0..12 {
            let mut seen: Vec<Vec<u64>> = vec![Vec::new(); self.blanks];
            for q in &self.quads {
                tick.tick()?;
                let [s, p, o, g] = *q;
                let (hs, ho, hg) = (of(s, &colour), of(o, &colour), of(g, &colour));
                if s & BLANK != 0 {
                    seen[(s & !BLANK) as usize].push(mix(&(0u8, p, ho, hg)));
                }
                if o & BLANK != 0 {
                    seen[(o & !BLANK) as usize].push(mix(&(1u8, p, hs, hg)));
                }
                if g & BLANK != 0 {
                    seen[(g & !BLANK) as usize].push(mix(&(2u8, p, hs, ho)));
                }
            }
            for (c, mut v) in colour.iter_mut().zip(seen) {
                tick.tick()?;
                v.sort_unstable();
                *c = mix(&(*c, v));
            }
        }
        let mut hashes: Vec<u64> = self
            .quads
            .iter()
            .map(|&[s, p, o, g]| mix(&(of(s, &colour), p, of(o, &colour), of(g, &colour))))
            .collect();
        hashes.sort_unstable();
        Ok((colour, hashes))
    }
}

/// A backtracking search for a bijection of the blank nodes of `a` onto those of `b`
/// that maps every quad of `a` onto a quad of `b`. A blank node may only take a partner
/// of its own colour, and a quad is checked as soon as its blank nodes are all mapped.
/// `Ok(None)` when `budget` steps were not enough.
fn search(
    a: &Side,
    ca: &[u64],
    b: &Side,
    cb: &[u64],
    budget: usize,
    deadline: Option<Instant>,
) -> Result<Option<bool>, FormatError> {
    let mut tick = Ticker::new(deadline);
    let b_set: HashSet<[Code; 4]> = b.quads.iter().copied().collect();
    let (mut sa, mut sb) = (ca.to_vec(), cb.to_vec());
    sa.sort_unstable();
    sb.sort_unstable();
    if a.blanks != b.blanks || sa != sb {
        return Ok(Some(false));
    }
    // a quad is checked when its last blank node is mapped, and one without blank nodes
    // must be there as it is
    let mut due: Vec<Vec<usize>> = vec![Vec::new(); a.blanks];
    for (qi, q) in a.quads.iter().enumerate() {
        match [q[0], q[2], q[3]]
            .iter()
            .filter(|c| **c & BLANK != 0)
            .map(|c| (*c & !BLANK) as usize)
            .max()
        {
            Some(last) => due[last].push(qi),
            None if !b_set.contains(q) => return Ok(Some(false)),
            None => {}
        }
    }
    let mut candidates: HashMap<u64, Vec<usize>> = HashMap::new();
    for (y, c) in cb.iter().enumerate() {
        candidates.entry(*c).or_default().push(y);
    }
    let mut map = vec![0 as Code; a.blanks];
    let mut used = vec![false; b.blanks];
    let mut next = vec![0usize; a.blanks + 1];
    let mut spent = 0usize;
    let mut depth = 0usize;
    let image = |c: Code, map: &[Code]| {
        if c & BLANK != 0 {
            BLANK | map[(c & !BLANK) as usize]
        } else {
            c
        }
    };
    loop {
        if depth == a.blanks {
            return Ok(Some(true));
        }
        let Some(cands) = candidates.get(&ca[depth]) else {
            return Ok(Some(false));
        };
        let mut placed = false;
        while next[depth] < cands.len() {
            tick.tick()?;
            let y = cands[next[depth]];
            next[depth] += 1;
            spent += 1;
            if used[y] {
                continue;
            }
            map[depth] = y as Code;
            let mut ok = true;
            for &qi in &due[depth] {
                tick.tick()?;
                spent += 1;
                let [s, p, o, g] = a.quads[qi];
                if !b_set.contains(&[image(s, &map), p, image(o, &map), image(g, &map)]) {
                    ok = false;
                    break;
                }
            }
            if spent > budget {
                return Ok(None);
            }
            if ok {
                used[y] = true;
                placed = true;
                break;
            }
        }
        if placed {
            depth += 1;
            next[depth] = 0;
        } else if depth == 0 {
            return Ok(Some(false));
        } else {
            depth -= 1;
            used[map[depth] as usize] = false;
        }
    }
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

    fn bounded(a: &[Quad], b: &[Quad]) -> Option<bool> {
        isomorphic_bounded(a, b, None).unwrap()
    }

    fn within(a: &[Quad], b: &[Quad], budget: usize) -> Option<bool> {
        isomorphic_within(a, b, budget, None).unwrap()
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
            bounded(&children(&fwd, 11), &children(&rev, 11)),
            Some(true)
        );
        // one child fewer on one side
        assert_eq!(
            bounded(&children(&fwd, 11), &children(&fwd[1..], 11)),
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
        assert_eq!(bounded(&children(&fwd, 11), &other), Some(false));
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
        assert_eq!(bounded(&a, &shuffled), Some(true));
        assert_eq!(bounded(&a, &lists(30, ["y", "x"])), Some(false));
        // ten identical blank nodes are not nine
        let ten = children(&(0..10).collect::<Vec<_>>(), 10);
        let nine = children(&(0..9).collect::<Vec<_>>(), 9);
        assert_eq!(bounded(&ten, &nine), Some(false));
        // and duplicates of one quad do not make a difference
        let mut dup = ten.clone();
        dup.extend(ten.clone());
        assert_eq!(bounded(&ten, &dup), Some(true));
    }

    #[test]
    fn an_exhausted_budget_is_not_an_answer() {
        let fwd: Vec<usize> = (0..22).collect();
        // not a mirror image, which numbers the blank nodes alike
        let rev: Vec<usize> = (0..22).step_by(2).chain((1..22).step_by(2)).collect();
        let (a, b) = (children(&fwd, 11), children(&rev, 11));
        assert_eq!(within(&a, &b, 5), None);
        assert_eq!(within(&a, &b, SEARCH_BUDGET), Some(true));
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

    #[test]
    fn the_deadline_stops_a_comparison_with_large_terms() {
        // two lists sharing 120 copies of a long literal, with exchanged tails, which
        // the fast path cannot settle
        let big = "x".repeat(8192);
        let list = |k: usize, end: &str| -> String {
            let mut s = format!("<http://e/s> <http://e/p{k}> _:l{k}_0 .\n");
            for i in 0..120 {
                s.push_str(&format!(
                    "_:l{k}_{i} <http://e/first> \"{big}\" . _:l{k}_{i} <http://e/rest> _:l{k}_{} .\n",
                    i + 1
                ));
            }
            s.push_str(&format!(
                "_:l{k}_120 <http://e/first> <http://e/{end}> . _:l{k}_120 <http://e/rest> <http://e/nil> .\n"
            ));
            s
        };
        let a = quads(&(list(0, "x") + &list(1, "y")), Language::Turtle);
        let b = quads(&(list(0, "y") + &list(1, "x")), Language::Turtle);
        let start = std::time::Instant::now();
        // an expired deadline is a timeout, not an answer
        let past = Instant::now() - std::time::Duration::from_secs(1);
        assert_eq!(
            isomorphic_bounded(&a, &b, Some(past)),
            Err(FormatError::Timeout)
        );
        // without one the comparison is quick, because terms are interned once
        assert_eq!(bounded(&a, &b), Some(false));
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }
}
