//! Property paths, SPARQL 1.1 section 18.4. A path evaluates to the pairs of nodes that it
//! connects in the active graph. Sequence and alternative keep their duplicates, and the
//! closure operators produce each pair once.

use super::{Evaluator, GraphData, R, unsupported};
use oxrdf::Term;
use spargebra::algebra::PropertyPathExpression as P;
use std::collections::{HashMap, HashSet};

type Pairs = Vec<(Term, Term)>;

impl Evaluator<'_> {
    /// The pairs that `path` connects in `graph`. `extra` are the terms that the pattern
    /// names, which are nodes of the zero-length paths whether the graph holds them or not.
    pub(crate) fn path_pairs(&self, path: &P, graph: &GraphData, extra: &[Term]) -> R<Pairs> {
        Ok(match path {
            P::NamedNode(p) => graph
                .by_predicate
                .get(p)
                .into_iter()
                .flatten()
                .map(|&i| {
                    let t = &graph.triples[i];
                    (Term::from(t.subject.clone()), t.object.clone())
                })
                .collect(),
            P::Reverse(p) => self
                .path_pairs(p, graph, extra)?
                .into_iter()
                .map(|(a, b)| (b, a))
                .collect(),
            P::Sequence(a, b) => {
                let left = self.path_pairs(a, graph, extra)?;
                let right = self.path_pairs(b, graph, extra)?;
                let mut by_start: HashMap<&Term, Vec<&Term>> = HashMap::new();
                for (s, e) in &right {
                    by_start.entry(s).or_default().push(e);
                }
                let mut out = Vec::new();
                for (s, m) in &left {
                    for e in by_start.get(m).into_iter().flatten() {
                        out.push((s.clone(), (*e).clone()));
                    }
                }
                out
            }
            P::Alternative(a, b) => {
                let mut out = self.path_pairs(a, graph, extra)?;
                out.extend(self.path_pairs(b, graph, extra)?);
                out
            }
            P::ZeroOrOne(p) => {
                let mut seen = HashSet::new();
                let mut out = Vec::new();
                for n in nodes(graph, extra) {
                    if seen.insert((n.clone(), n.clone())) {
                        out.push((n.clone(), n));
                    }
                }
                for pair in self.path_pairs(p, graph, extra)? {
                    if seen.insert(pair.clone()) {
                        out.push(pair);
                    }
                }
                out
            }
            P::ZeroOrMore(p) => self.closure(p, graph, extra, true)?,
            P::OneOrMore(p) => self.closure(p, graph, extra, false)?,
            P::NegatedPropertySet(excluded) => graph
                .triples
                .iter()
                .filter(|t| !excluded.contains(&t.predicate))
                .map(|t| (Term::from(t.subject.clone()), t.object.clone()))
                .collect(),
            P::Range { .. } => return unsupported("a Jena path range"),
            P::Distinct(_) => return unsupported("a Jena distinct() path"),
            P::Multi(_) => return unsupported("a Jena multi() path"),
            P::Shortest(_) => return unsupported("a Jena shortest() path"),
        })
    }

    /// `p*` when `zero` and `p+` otherwise: the pairs of nodes joined by walks of the
    /// pairs of `p`, each once.
    fn closure(&self, p: &P, graph: &GraphData, extra: &[Term], zero: bool) -> R<Pairs> {
        let step = self.path_pairs(p, graph, extra)?;
        let mut next: HashMap<&Term, Vec<&Term>> = HashMap::new();
        for (a, b) in &step {
            next.entry(a).or_default().push(b);
        }
        let starts: Vec<Term> = if zero {
            nodes(graph, extra)
        } else {
            let mut seen = HashSet::new();
            step.iter()
                .map(|(a, _)| a.clone())
                .filter(|a| seen.insert(a.clone()))
                .collect()
        };
        let mut out = Vec::new();
        for start in starts {
            let mut reached: HashSet<Term> = HashSet::new();
            let mut order = Vec::new();
            if zero {
                reached.insert(start.clone());
                order.push(start.clone());
            }
            let mut frontier = vec![start.clone()];
            while let Some(n) = frontier.pop() {
                for m in next.get(&n).into_iter().flatten() {
                    if reached.insert((*m).clone()) {
                        order.push((*m).clone());
                        frontier.push((*m).clone());
                    }
                }
            }
            for end in order {
                out.push((start.clone(), end));
            }
        }
        Ok(out)
    }
}

/// The nodes of the graph and the extra terms, each once.
fn nodes(graph: &GraphData, extra: &[Term]) -> Vec<Term> {
    let mut all = graph.nodes();
    let seen: HashSet<Term> = all.iter().cloned().collect();
    for t in extra {
        if !seen.contains(t) && !all.contains(t) {
            all.push(t.clone());
        }
    }
    all
}
