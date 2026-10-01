//! Recognition of the `text:query` property function (Jena's full-text search syntax) in
//! a basic graph pattern.
//!
//! ```sparql
//! ?s text:query "query"                          # or "query"@lang
//! (?s ?score ?literal ?g ?prop) text:query (pred* "query" limit "lang:xx")
//! ```
//!
//! SPARQL parses `( … )` into `rdf:first` / `rdf:rest` chains of blank nodes. This module
//! takes each `text:query` triple and the chain triples of its arguments out of the BGP
//! and decodes them into a [`TextCall`]; the planner turns that into a search leaf.

use crate::error::{Error, Result};
use oxrdf::vocab::{rdf, xsd};
use oxrdf::{Literal, NamedNode};
use rustc_hash::FxHashMap;
use spargebra::term::{BlankNode, NamedNodePattern, TermPattern, TriplePattern};

/// `text:query` (Jena's IRI, so existing queries run unchanged).
pub const TEXT_QUERY: &str = "http://jena.apache.org/text#query";

/// A decoded `text:query` call.
#[derive(Clone, Debug)]
pub struct TextCall {
    /// the subject slot: a variable or a constant (a constant restricts the search)
    pub subject: TermPattern,
    pub score: Option<TermPattern>,
    pub literal: Option<TermPattern>,
    pub graph: Option<TermPattern>,
    pub prop: Option<TermPattern>,
    /// predicates to search (empty: every indexed predicate)
    pub predicates: Vec<NamedNode>,
    pub query: String,
    pub lang: Option<String>,
    /// `Some(n)` keeps the top `n` hits
    pub limit: Option<usize>,
}

fn bad(msg: impl Into<String>) -> Error {
    Error::invalid(format!("text:query: {}", msg.into()))
}

/// Take the `text:query` calls out of `patterns`; returns the calls and the remaining
/// triple patterns.
pub fn extract(patterns: &[TriplePattern]) -> Result<(Vec<TextCall>, Vec<TriplePattern>)> {
    let (calls, rest) = take_calls(patterns, TEXT_QUERY, "text:query")?;
    let calls = calls
        .into_iter()
        .map(|(s, o)| decode(s, o))
        .collect::<Result<_>>()?;
    Ok((calls, rest))
}

/// Take the triples whose predicate is the property function `iri` out of `patterns`,
/// together with the `rdf:first`/`rdf:rest` triples of their list arguments. Returns each
/// call's subject and object elements (a single element when the argument is not a
/// list), and the remaining patterns.
#[allow(clippy::type_complexity)]
pub fn take_calls(
    patterns: &[TriplePattern],
    iri: &str,
    name: &str,
) -> Result<(
    Vec<(Vec<TermPattern>, Vec<TermPattern>)>,
    Vec<TriplePattern>,
)> {
    let (calls, rest) = take_calls_where(patterns, |p| p == iri, |_| name.to_string())?;
    Ok((calls.into_iter().map(|(_, s, o)| (s, o)).collect(), rest))
}

/// [`take_calls`] for every property function whose IRI satisfies `is_call`; each call
/// comes with its predicate. Errors are prefixed with `name(iri)`.
#[allow(clippy::type_complexity)]
pub fn take_calls_where(
    patterns: &[TriplePattern],
    is_call: impl Fn(&str) -> bool,
    name: impl Fn(&str) -> String,
) -> Result<(
    Vec<(NamedNode, Vec<TermPattern>, Vec<TermPattern>)>,
    Vec<TriplePattern>,
)> {
    let call_iri = |t: &TriplePattern| match &t.predicate {
        NamedNodePattern::NamedNode(p) if is_call(p.as_str()) => Some(p.clone()),
        _ => None,
    };
    if !patterns.iter().any(|t| call_iri(t).is_some()) {
        return Ok((Vec::new(), patterns.to_vec()));
    }
    let bad = |iri: &str| Error::invalid(format!("{}: malformed argument list", name(iri)));
    // list cells: blank node → (first, rest) triple indexes
    let mut first: FxHashMap<&BlankNode, Vec<usize>> = FxHashMap::default();
    let mut rest: FxHashMap<&BlankNode, Vec<usize>> = FxHashMap::default();
    for (i, t) in patterns.iter().enumerate() {
        if let (TermPattern::BlankNode(b), NamedNodePattern::NamedNode(p)) =
            (&t.subject, &t.predicate)
        {
            if *p == rdf::FIRST {
                first.entry(b).or_default().push(i);
            } else if *p == rdf::REST {
                rest.entry(b).or_default().push(i);
            }
        }
    }
    let mut used = vec![false; patterns.len()];
    // the elements of the list headed by `t`, if `t` heads one
    let list =
        |t: &TermPattern, used: &mut Vec<bool>, iri: &str| -> Result<Option<Vec<TermPattern>>> {
            let TermPattern::BlankNode(b) = t else {
                return Ok(None);
            };
            let mut b = b;
            if !first.contains_key(b) {
                return Ok(None);
            }
            let mut items = Vec::new();
            loop {
                let (Some([f]), Some([r])) = (
                    first.get(b).map(Vec::as_slice),
                    rest.get(b).map(Vec::as_slice),
                ) else {
                    return Err(bad(iri));
                };
                used[*f] = true;
                used[*r] = true;
                items.push(patterns[*f].object.clone());
                match &patterns[*r].object {
                    TermPattern::NamedNode(n) if *n == rdf::NIL => return Ok(Some(items)),
                    TermPattern::BlankNode(next) if items.len() < 64 => b = next,
                    _ => return Err(bad(iri)),
                }
            }
        };
    let mut calls = Vec::new();
    for (i, t) in patterns.iter().enumerate() {
        let Some(iri) = call_iri(t) else {
            continue;
        };
        used[i] = true;
        let subjects =
            list(&t.subject, &mut used, iri.as_str())?.unwrap_or_else(|| vec![t.subject.clone()]);
        let args =
            list(&t.object, &mut used, iri.as_str())?.unwrap_or_else(|| vec![t.object.clone()]);
        calls.push((iri, subjects, args));
    }
    let rest = patterns
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|(t, _)| t.clone())
        .collect();
    Ok((calls, rest))
}

fn decode(subjects: Vec<TermPattern>, args: Vec<TermPattern>) -> Result<TextCall> {
    if subjects.is_empty() || subjects.len() > 5 {
        return Err(bad("malformed argument list"));
    }
    let mut slots = subjects.into_iter();
    let subject = slots.next().unwrap();
    let mut var_slot = |name: &str| -> Result<Option<TermPattern>> {
        match slots.next() {
            None => Ok(None),
            Some(v @ (TermPattern::Variable(_) | TermPattern::BlankNode(_))) => Ok(Some(v)),
            Some(_) => Err(bad(format!("{name} must be a variable"))),
        }
    };
    let score = var_slot("the score")?;
    let literal = var_slot("the literal")?;
    let graph = var_slot("the graph")?;
    let prop = var_slot("the property")?;

    let mut predicates = Vec::new();
    let mut args = args.into_iter().peekable();
    while let Some(TermPattern::NamedNode(p)) = args.peek() {
        predicates.push(p.clone());
        args.next();
    }
    let string = |l: &Literal| -> Option<(String, Option<String>)> {
        if let Some(lang) = l.language() {
            return Some((l.value().to_string(), Some(lang.to_ascii_lowercase())));
        }
        (l.datatype() == xsd::STRING).then(|| (l.value().to_string(), None))
    };
    let (query, mut lang) = match args.next() {
        Some(TermPattern::Literal(l)) => {
            string(&l).ok_or_else(|| bad("query string must be a string literal"))?
        }
        Some(TermPattern::Variable(_) | TermPattern::BlankNode(_)) => {
            return Err(bad("arguments must be constants"));
        }
        Some(_) => return Err(bad("query string must be a string literal")),
        None => return Err(bad("malformed argument list")),
    };
    let mut limit = None;
    let rest: Vec<TermPattern> = args.collect();
    if rest.len() > 2 {
        return Err(bad("malformed argument list"));
    }
    for a in rest {
        let TermPattern::Literal(l) = a else {
            return Err(match a {
                TermPattern::Variable(_) | TermPattern::BlankNode(_) => {
                    bad("arguments must be constants")
                }
                _ => bad("malformed argument list"),
            });
        };
        if let Some(tag) = l.value().strip_prefix("lang:")
            && l.datatype() == xsd::STRING
        {
            lang = Some(tag.to_ascii_lowercase());
        } else if l.value().starts_with("highlight:") {
            return Err(bad("highlight is not supported yet"));
        } else if let Ok(n) = l.value().parse::<i64>()
            && l.datatype() != xsd::STRING
        {
            limit = (n > 0).then_some(n as usize);
        } else {
            return Err(bad(format!("unexpected argument {l}")));
        }
    }
    Ok(TextCall {
        subject,
        score,
        literal,
        graph,
        prop,
        predicates,
        query,
        lang,
        limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use spargebra::algebra::GraphPattern;
    use spargebra::{Query, SparqlParser};

    fn bgp(q: &str) -> Vec<TriplePattern> {
        let q = SparqlParser::new()
            .with_prefix("text", "http://jena.apache.org/text#")
            .unwrap()
            .with_prefix("rdfs", "http://www.w3.org/2000/01/rdf-schema#")
            .unwrap()
            .parse_query(q)
            .unwrap();
        let Query::Select { pattern, .. } = q else {
            panic!()
        };
        fn find(p: &GraphPattern) -> Option<Vec<TriplePattern>> {
            match p {
                GraphPattern::Bgp { patterns } => Some(patterns.clone()),
                GraphPattern::Project { inner, .. }
                | GraphPattern::Distinct { inner }
                | GraphPattern::Slice { inner, .. } => find(inner),
                _ => None,
            }
        }
        find(&pattern).unwrap()
    }

    #[test]
    fn decodes_the_jena_forms() {
        let (calls, rest) = extract(&bgp("SELECT * { ?s text:query \"fox\" . ?s a ?t }")).unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(calls[0].query, "fox");
        assert!(calls[0].predicates.is_empty() && calls[0].limit.is_none());

        let (calls, rest) = extract(&bgp(
            "SELECT * { (?s ?sc ?lit) text:query (rdfs:label rdfs:comment \"brown\" 10 \"lang:EN\") }",
        ))
        .unwrap();
        assert!(rest.is_empty());
        let c = &calls[0];
        assert_eq!(c.predicates.len(), 2);
        assert_eq!((c.limit, c.lang.as_deref()), (Some(10), Some("en")));
        assert!(c.score.is_some() && c.literal.is_some() && c.graph.is_none());

        let (calls, _) = extract(&bgp("SELECT * { ?s text:query \"renard\"@fr }")).unwrap();
        assert_eq!(calls[0].lang.as_deref(), Some("fr"));
    }

    #[test]
    fn rejects_malformed_calls() {
        for q in [
            "SELECT * { ?s text:query 42 }",
            "SELECT * { (?s 1) text:query \"x\" }",
            "SELECT * { ?s text:query (?q) }",
            "SELECT * { ?s text:query (\"x\" 1 2 3) }",
            "SELECT * { ?s text:query (\"x\" \"highlight:\") }",
        ] {
            assert!(extract(&bgp(q)).is_err(), "{q}");
        }
    }
}
