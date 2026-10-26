//! Bounded dependency production for directly anchored SHACL-SPARQL patterns.

use super::analysis_control::{self as admission, charge};
use super::{Dir, Fallback, PropertyPath};
use crate::localize::Read;
use spargebra::{
    Query,
    algebra::{Expression, GraphPattern, PropertyPathExpression},
    term::{NamedNodePattern, TermPattern},
};

type Control<'a> = Option<&'a mut dyn FnMut() -> Result<(), Fallback>>;

pub(super) enum Reads {
    Local(Vec<Read>),
    Global,
    Unsupported,
}

fn anchor<'a>(
    term: &TermPattern,
    anchors: &'a [(&str, Vec<PropertyPath>)],
) -> Option<&'a [PropertyPath]> {
    let TermPattern::Variable(variable) = term else {
        return None;
    };
    anchors
        .iter()
        .find(|(name, _)| *name == variable.as_str())
        .map(|(_, path)| path.as_slice())
}

fn pure(
    expression: &Expression,
    control: &mut Control<'_>,
    depth: usize,
) -> Result<bool, Fallback> {
    charge(control)?;
    if depth > 64 {
        return Ok(false);
    }
    use Expression as E;
    Ok(match expression {
        E::NamedNode(_) | E::Literal(_) | E::Variable(_) | E::Bound(_) => true,
        E::Exists(_) => false,
        E::Or(a, b)
        | E::And(a, b)
        | E::Equal(a, b)
        | E::SameTerm(a, b)
        | E::Greater(a, b)
        | E::GreaterOrEqual(a, b)
        | E::Less(a, b)
        | E::LessOrEqual(a, b)
        | E::Add(a, b)
        | E::Subtract(a, b)
        | E::Multiply(a, b)
        | E::Divide(a, b) => pure(a, control, depth + 1)? && pure(b, control, depth + 1)?,
        E::UnaryPlus(a) | E::UnaryMinus(a) | E::Not(a) => pure(a, control, depth + 1)?,
        E::In(a, xs) => {
            let mut safe = pure(a, control, depth + 1)?;
            for x in xs {
                safe &= pure(x, control, depth + 1)?;
            }
            safe
        }
        E::If(a, b, c) => {
            pure(a, control, depth + 1)?
                && pure(b, control, depth + 1)?
                && pure(c, control, depth + 1)?
        }
        E::Coalesce(xs) | E::FunctionCall(_, xs) => {
            let mut safe = true;
            for x in xs {
                safe &= pure(x, control, depth + 1)?;
            }
            safe
        }
    })
}

fn path(
    expression: &PropertyPathExpression,
    control: &mut Control<'_>,
    depth: usize,
) -> Result<Option<PropertyPath>, Fallback> {
    charge(control)?;
    if depth > 64 {
        return Ok(None);
    }
    use PropertyPath as P;
    use PropertyPathExpression as E;
    Ok(Some(match expression {
        E::NamedNode(n) => {
            admission::admit_iri(n, control)?;
            P::Predicate(n.clone())
        }
        E::Reverse(p) => {
            let Some(p) = path(p, control, depth + 1)? else {
                return Ok(None);
            };
            P::Inverse(Box::new(p))
        }
        E::Sequence(a, b) | E::Alternative(a, b) => {
            let (Some(a), Some(b)) = (path(a, control, depth + 1)?, path(b, control, depth + 1)?)
            else {
                return Ok(None);
            };
            if matches!(expression, E::Sequence(..)) {
                P::Sequence(vec![a, b])
            } else {
                P::Alternative(vec![a, b])
            }
        }
        E::ZeroOrMore(p) => {
            let Some(p) = path(p, control, depth + 1)? else {
                return Ok(None);
            };
            P::ZeroOrMore(Box::new(p))
        }
        E::OneOrMore(p) => {
            let Some(p) = path(p, control, depth + 1)? else {
                return Ok(None);
            };
            P::OneOrMore(Box::new(p))
        }
        E::ZeroOrOne(p) => {
            let Some(p) = path(p, control, depth + 1)? else {
                return Ok(None);
            };
            P::ZeroOrOne(Box::new(p))
        }
        _ => return Ok(None),
    }))
}

pub(super) fn reads(
    query: &Query,
    anchors: &[(&str, Vec<PropertyPath>)],
    control: &mut Control<'_>,
) -> Result<Reads, Fallback> {
    let (Query::Select {
        pattern, dataset, ..
    }
    | Query::Ask {
        pattern, dataset, ..
    }
    | Query::Construct {
        pattern, dataset, ..
    }
    | Query::Describe {
        pattern, dataset, ..
    }) = query;
    charge(control)?;
    if dataset.is_some() {
        return Ok(Reads::Unsupported);
    }
    let mut pattern = pattern;
    loop {
        charge(control)?;
        pattern = match pattern {
            GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Reduced { inner }
            | GraphPattern::Slice { inner, .. } => inner,
            _ => break,
        };
    }
    analyze(pattern, anchors, control, 0)
}

fn analyze(
    pattern: &GraphPattern,
    anchors: &[(&str, Vec<PropertyPath>)],
    control: &mut Control<'_>,
    depth: usize,
) -> Result<Reads, Fallback> {
    charge(control)?;
    if depth > 64 {
        return Ok(Reads::Unsupported);
    }
    use GraphPattern as G;
    Ok(match pattern {
        G::Bgp { patterns } => {
            let mut out = Vec::new();
            let mut unanchored = false;
            for triple in patterns {
                charge(control)?;
                let (prefix, forward) = if let Some(p) = anchor(&triple.subject, anchors) {
                    (p, true)
                } else if let Some(p) = anchor(&triple.object, anchors) {
                    (p, false)
                } else {
                    unanchored = true;
                    continue;
                };
                let pred = match &triple.predicate {
                    NamedNodePattern::NamedNode(n) => {
                        admission::admit_iri(n, control)?;
                        Some(n.clone())
                    }
                    NamedNodePattern::Variable(_) => None,
                };
                let prefix = admission::prefix(prefix, control)?;
                charge(control)?;
                out.push(Read {
                    prefix,
                    pred,
                    out: forward,
                });
            }
            if unanchored {
                if out.is_empty() {
                    Reads::Global
                } else {
                    Reads::Unsupported
                }
            } else {
                Reads::Local(out)
            }
        }
        G::Path {
            subject,
            path: expression,
            object,
        } => {
            let (prefix, forward) = if let Some(p) = anchor(subject, anchors) {
                (p, true)
            } else if let Some(p) = anchor(object, anchors) {
                (p, false)
            } else {
                return Ok(Reads::Global);
            };
            let Some(path) = path(expression, control, 0)? else {
                return Ok(Reads::Unsupported);
            };
            let normalized = admission::nnf(&path, !forward, control)?;
            let mut out = Vec::new();
            for (tail, pred, dir) in admission::steps(&normalized, control)? {
                let mut full = admission::prefix(prefix, control)?;
                full.extend(tail);
                charge(control)?;
                out.push(Read {
                    prefix: full,
                    pred: Some(pred),
                    out: dir == Dir::Out,
                });
            }
            Reads::Local(out)
        }
        G::Filter { expr, inner } => {
            let reads = analyze(inner, anchors, control, depth + 1)?;
            if matches!(reads, Reads::Global) {
                Reads::Global
            } else if pure(expr, control, 0)? {
                reads
            } else {
                Reads::Unsupported
            }
        }
        // These already require global analysis in the ordinary guard. No eager
        // localization producer is needed to classify their scope.
        G::Graph { .. }
        | G::Service { .. }
        | G::Minus { .. }
        | G::Values { .. }
        | G::Project { .. } => Reads::Global,
        _ => Reads::Unsupported,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Query {
        spargebra::SparqlParser::new().parse_query(text).unwrap()
    }

    #[test]
    fn directly_anchored_queries_match_existing_dependency_reads() {
        for text in [
            "SELECT $this WHERE { $this <urn:p> ?value }",
            "SELECT $this WHERE { ?value <urn:p> $this }",
            "SELECT $this WHERE { $this <urn:p> ?value FILTER(?value != 1) }",
            "SELECT $this WHERE { $this (^<urn:p>/<urn:q>*)* ?value }",
        ] {
            let query = parse(text);
            let anchors = [("this", Vec::new())];
            let expected = crate::localize::reads(&query, &anchors).unwrap();
            let mut units = 0;
            let mut check = || {
                units += 1;
                Ok(())
            };
            let Reads::Local(actual) = reads(&query, &anchors, &mut Some(&mut check)).unwrap()
            else {
                panic!("direct anchor was not localized: {text}")
            };
            assert_eq!(actual, expected, "{text}");
            assert!(units > 0);
        }
    }

    #[test]
    fn bounded_query_production_observes_control_before_large_path_copies() {
        let path = std::iter::repeat_n("<urn:p>", 300)
            .collect::<Vec<_>>()
            .join("/");
        let query = parse(&format!("SELECT $this WHERE {{ $this ({path})* ?value }}"));
        let mut units = 0;
        let mut check = || {
            units += 1;
            if units > 20 {
                Err(Fallback::Budget)
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            reads(&query, &[("this", Vec::new())], &mut Some(&mut check)),
            Err(Fallback::Budget)
        ));
        assert_eq!(units, 21);
    }
    #[test]
    fn excessive_query_path_depth_refuses_the_producer_without_broadening_scope() {
        let path = std::iter::repeat_n("<urn:p>", 100)
            .collect::<Vec<_>>()
            .join("/");
        let query = parse(&format!("SELECT $this WHERE {{ $this ({path})* ?value }}"));
        let mut check = || Ok(());
        assert!(matches!(
            reads(&query, &[("this", Vec::new())], &mut Some(&mut check)),
            Ok(Reads::Unsupported)
        ));
    }
}
