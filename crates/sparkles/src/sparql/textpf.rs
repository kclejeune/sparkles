//! Recognition of the `text:query` property function (Jena's full-text search syntax) in
//! a basic graph pattern.
//!
//! ```sparql
//! ?s text:query "query"                          # or "query"@lang
//! (?s ?score ?literal ?g ?prop ?rank) text:query (pred* "query" limit "lang:xx")
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
    /// the hit's rank in the score order (a Sparkles extension after Jena's slots)
    pub rank: Option<TermPattern>,
    /// predicates to search (empty: every indexed predicate)
    pub predicates: Vec<NamedNode>,
    pub query: String,
    pub lang: Option<String>,
    /// `Some(n)` keeps the top `n` hits
    pub limit: Option<usize>,
    /// `"highlight:…"`: the literal output becomes the highlighted fragments
    pub highlight: Option<crate::text::HighlightOpts>,
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

/// The elements of the list headed by `head`, taking its `rdf:first`/`rdf:rest` triples
/// out of `patterns`; `None` when `head` heads no list (it is then left as it is). A
/// nested list argument of a property function stays in the patterns until taken so.
pub fn take_list(
    patterns: &mut Vec<TriplePattern>,
    head: &TermPattern,
    name: &str,
) -> Result<Option<Vec<TermPattern>>> {
    let TermPattern::BlankNode(b) = head else {
        return Ok(None);
    };
    let link = |patterns: &[TriplePattern], b: &BlankNode, p: oxrdf::NamedNodeRef<'_>| {
        patterns
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                matches!(&t.subject, TermPattern::BlankNode(x) if x == b)
                    && matches!(&t.predicate, NamedNodePattern::NamedNode(x) if *x == p)
            })
            .map(|(i, _)| i)
            .collect::<Vec<usize>>()
    };
    if link(patterns, b, rdf::FIRST).is_empty() {
        return Ok(None);
    }
    let bad = || Error::invalid(format!("{name}: malformed argument list"));
    let mut used = Vec::new();
    let mut items = Vec::new();
    let mut b = b.clone();
    loop {
        let (first, rest) = (
            link(patterns, &b, rdf::FIRST),
            link(patterns, &b, rdf::REST),
        );
        let ([f], [r]) = (first.as_slice(), rest.as_slice()) else {
            return Err(bad());
        };
        used.extend([*f, *r]);
        items.push(patterns[*f].object.clone());
        match &patterns[*r].object {
            TermPattern::NamedNode(n) if *n == rdf::NIL => break,
            TermPattern::BlankNode(next) if items.len() < 64 => b = next.clone(),
            _ => return Err(bad()),
        }
    }
    used.sort_unstable();
    for i in used.into_iter().rev() {
        patterns.remove(i);
    }
    Ok(Some(items))
}

/// Decode a `text:query` call from its subject and object list elements.
pub fn decode(subjects: Vec<TermPattern>, args: Vec<TermPattern>) -> Result<TextCall> {
    if subjects.is_empty() || subjects.len() > 6 {
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
    let rank = var_slot("the rank")?;

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
    let mut highlight = None;
    let rest: Vec<TermPattern> = args.collect();
    if rest.len() > 3 {
        return Err(bad("malformed argument list"));
    }
    // each of limit, lang: and highlight: at most once
    let mut seen = [false; 3];
    let mut once = |i: usize| {
        if std::mem::replace(&mut seen[i], true) {
            Err(bad("malformed argument list"))
        } else {
            Ok(())
        }
    };
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
            once(0)?;
            lang = Some(tag.to_ascii_lowercase());
        } else if let Some(opts) = l.value().strip_prefix("highlight:")
            && l.datatype() == xsd::STRING
        {
            once(1)?;
            highlight = Some(crate::text::HighlightOpts::parse(opts).map_err(bad)?);
        } else if let Ok(n) = l.value().parse::<i64>()
            && l.datatype() != xsd::STRING
        {
            once(2)?;
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
        rank,
        predicates,
        query,
        lang,
        limit,
        highlight,
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
            "SELECT * { ?s text:query (\"x\" 1 2) }",
            "SELECT * { ?s text:query (\"x\" \"highlight:\" \"highlight:\") }",
            "SELECT * { ?s text:query (\"x\" \"highlight:q:1\") }",
            "SELECT * { ?s text:query (\"x\" \"highlight:z:0\") }",
            "SELECT * { ?s text:query (\"x\" \"highlight:jh:maybe\") }",
        ] {
            assert!(extract(&bgp(q)).is_err(), "{q}");
        }
    }

    #[test]
    fn decodes_highlight_options() {
        let (calls, _) = extract(&bgp(
            "SELECT * { (?s ?sc ?lit) text:query (\"x\" 10 \"lang:en\" \"highlight:\") }",
        ))
        .unwrap();
        assert_eq!(calls[0].highlight, Some(Default::default()));
        let (calls, _) = extract(&bgp(
            "SELECT * { (?s ?sc ?lit) text:query (\"x\" \"highlight:s:<em class='hiLite'> | e:</em> | z:30 | m:1 | jh:n | jf:y | f: … \") }",
        ))
        .unwrap();
        let h = calls[0].highlight.clone().unwrap();
        assert_eq!(
            (h.start.as_str(), h.end.as_str(), h.frag_size, h.max_frags),
            ("<em class='hiLite'>", "</em>", 30, 1)
        );
        assert_eq!(
            (h.join_hi, h.join_frags, h.frag_sep.as_str()),
            (false, true, " …")
        );
    }
}
