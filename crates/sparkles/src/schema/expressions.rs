//! Anonymous class expressions, rendered as text in the OWL 2 Manchester Syntax.
//!
//! A blank-node object of `rdfs:subClassOf`, `owl:equivalentClass`, `rdfs:domain` or
//! `rdfs:range` is a class expression in the OWL 2 mapping to RDF graphs: a restriction
//! (`owl:Restriction` with `owl:onProperty`), a Boolean combination (`owl:unionOf`,
//! `owl:intersectionOf`, `owl:complementOf`), an enumeration (`owl:oneOf`) or a datatype
//! restriction (`owl:onDatatype` with `owl:withRestrictions`). The report renders each
//! one, nested ones included, with IRIs in angle brackets, as in
//! `<http://ex.org/hasPart> some (<http://ex.org/A> or <http://ex.org/B>)`. A node that
//! fits none of these shapes, or a nesting deeper than [`MAX_DEPTH`], is rendered as
//! `[…]`.

use super::{Budget, GraphFilter, Src};
use crate::id::Id;
use crate::index::Perm;
use oxrdf::Term;
use rustc_hash::FxHashMap;

/// The deepest nesting rendered.
pub const MAX_DEPTH: usize = 8;

/// The most members of one list rendered.
const MAX_LIST: usize = 64;

const OWL: &str = "http://www.w3.org/2002/07/owl#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

/// Renders the class expressions of blank nodes, reading their triples from the
/// declared graphs and remembering each node's text.
pub(super) struct Renderer<'a> {
    src: &'a Src<'a>,
    filter: &'a GraphFilter,
    budget: &'a Budget<'a>,
    memo: FxHashMap<u64, String>,
}

impl<'a> Renderer<'a> {
    pub(super) fn new(src: &'a Src<'a>, filter: &'a GraphFilter, budget: &'a Budget<'a>) -> Self {
        Renderer {
            src,
            filter,
            budget,
            memo: FxHashMap::default(),
        }
    }

    /// The `(predicate IRI, object)` pairs of a node in the declared graphs.
    fn arcs(&self, node: u64) -> crate::Result<Vec<(String, u64)>> {
        let mut ids: Vec<(u64, u64)> = Vec::new();
        self.src
            .for_each_key(Perm::Spo, &[node], self.budget, |k| {
                if self.filter.accepts(k[3]) && ids.last() != Some(&(k[1], k[2])) {
                    ids.push((k[1], k[2]));
                }
            })?;
        Ok(ids
            .into_iter()
            .filter_map(|(p, o)| match self.src.term(Id(p))? {
                Term::NamedNode(n) => Some((n.into_string(), o)),
                _ => None,
            })
            .collect())
    }

    fn one(arcs: &[(String, u64)], local: &str, ns: &str) -> Option<u64> {
        arcs.iter()
            .find(|(p, _)| {
                p.len() == ns.len() + local.len() && p.starts_with(ns) && p.ends_with(local)
            })
            .map(|(_, o)| *o)
    }

    /// The members of an RDF list.
    fn list(&self, mut head: u64) -> crate::Result<Option<Vec<u64>>> {
        let mut out = Vec::new();
        for _ in 0..=MAX_LIST {
            match self.src.term(Id(head)) {
                Some(Term::NamedNode(n)) if n.as_str() == format!("{RDF}nil") => {
                    return Ok(Some(out));
                }
                Some(Term::BlankNode(_)) => {}
                _ => return Ok(None),
            }
            let arcs = self.arcs(head)?;
            let (Some(first), Some(rest)) = (
                Self::one(&arcs, "first", RDF),
                Self::one(&arcs, "rest", RDF),
            ) else {
                return Ok(None);
            };
            out.push(first);
            head = rest;
        }
        Ok(None)
    }

    /// A term as an operand: an IRI in angle brackets, a literal in N-Triples, or a
    /// nested expression in parentheses.
    fn operand(&mut self, id: u64, depth: usize) -> crate::Result<String> {
        Ok(match self.src.term(Id(id)) {
            Some(Term::BlankNode(_)) => {
                let t = self.expr(id, depth + 1)?;
                if t.contains(' ') && !t.starts_with(['(', '{']) && !t.ends_with(']') {
                    format!("({t})")
                } else {
                    t
                }
            }
            Some(t) => t.to_string(),
            None => "[…]".into(),
        })
    }

    fn operands(&mut self, ids: &[u64], depth: usize, sep: &str) -> crate::Result<String> {
        let mut parts = Vec::with_capacity(ids.len());
        for &i in ids {
            parts.push(self.operand(i, depth)?);
        }
        Ok(parts.join(sep))
    }

    /// The text of the class expression at blank node `node`.
    pub(super) fn render(&mut self, node: u64) -> crate::Result<String> {
        self.expr(node, 0)
    }

    fn expr(&mut self, node: u64, depth: usize) -> crate::Result<String> {
        if let Some(t) = self.memo.get(&node) {
            return Ok(t.clone());
        }
        if depth > MAX_DEPTH {
            return Ok("[…]".into());
        }
        let arcs = self.arcs(node)?;
        let owl = |l: &str| Self::one(&arcs, l, OWL);
        let text = if let Some(p) = owl("onProperty") {
            let prop = match self.src.term(Id(p)) {
                Some(Term::BlankNode(_)) => {
                    let inner = self.arcs(p)?;
                    match Self::one(&inner, "inverseOf", OWL) {
                        Some(q) => format!("inverse {}", self.operand(q, depth)?),
                        None => "[…]".into(),
                    }
                }
                Some(t) => t.to_string(),
                None => "[…]".into(),
            };
            let qualified = |s: &mut Self, n: u64, word: &str| -> crate::Result<String> {
                let count = s.operand(n, depth)?;
                let count = count
                    .split('^')
                    .next()
                    .unwrap_or(&count)
                    .trim_matches('"')
                    .to_string();
                Ok(
                    match Self::one(&arcs, "onClass", OWL).or(Self::one(&arcs, "onDataRange", OWL))
                    {
                        Some(c) => format!("{prop} {word} {count} {}", s.operand(c, depth)?),
                        None => format!("{prop} {word} {count}"),
                    },
                )
            };
            if let Some(c) = owl("someValuesFrom") {
                format!("{prop} some {}", self.operand(c, depth)?)
            } else if let Some(c) = owl("allValuesFrom") {
                format!("{prop} only {}", self.operand(c, depth)?)
            } else if let Some(v) = owl("hasValue") {
                format!("{prop} value {}", self.operand(v, depth)?)
            } else if owl("hasSelf").is_some() {
                format!("{prop} Self")
            } else if let Some(n) = owl("minCardinality").or(owl("minQualifiedCardinality")) {
                qualified(self, n, "min")?
            } else if let Some(n) = owl("maxCardinality").or(owl("maxQualifiedCardinality")) {
                qualified(self, n, "max")?
            } else if let Some(n) = owl("cardinality").or(owl("qualifiedCardinality")) {
                qualified(self, n, "exactly")?
            } else {
                format!("{prop} […]")
            }
        } else if let Some(l) = owl("unionOf") {
            match self.list(l)? {
                Some(m) => format!("({})", self.operands(&m, depth, " or ")?),
                None => "[…]".into(),
            }
        } else if let Some(l) = owl("intersectionOf") {
            match self.list(l)? {
                Some(m) => format!("({})", self.operands(&m, depth, " and ")?),
                None => "[…]".into(),
            }
        } else if let Some(c) = owl("complementOf").or(owl("datatypeComplementOf")) {
            format!("not {}", self.operand(c, depth)?)
        } else if let Some(l) = owl("oneOf") {
            match self.list(l)? {
                Some(m) => format!("{{{}}}", self.operands(&m, depth, ", ")?),
                None => "[…]".into(),
            }
        } else if let (Some(dt), Some(r)) = (owl("onDatatype"), owl("withRestrictions")) {
            match self.list(r)? {
                Some(facets) => {
                    let mut parts = Vec::new();
                    for f in facets {
                        for (p, v) in self.arcs(f)? {
                            let facet = match p.strip_prefix(XSD) {
                                Some("minInclusive") => ">=".to_string(),
                                Some("minExclusive") => ">".to_string(),
                                Some("maxInclusive") => "<=".to_string(),
                                Some("maxExclusive") => "<".to_string(),
                                Some(other) => other.to_string(),
                                None => format!("<{p}>"),
                            };
                            parts.push(format!("{facet} {}", self.operand(v, depth)?));
                        }
                    }
                    format!("{}[{}]", self.operand(dt, depth)?, parts.join(", "))
                }
                None => "[…]".into(),
            }
        } else {
            "[…]".into()
        };
        self.memo.insert(node, text.clone());
        Ok(text)
    }
}
