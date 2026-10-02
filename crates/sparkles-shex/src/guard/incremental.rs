//! Incremental write-time ShEx validation: which associations of the shape map a write
//! can affect.
//!
//! Validating a node against a shape reads its neighbourhood: its arcs with the
//! predicates the schema's triple constraints mention (outgoing, and incoming for
//! inverse constraints; every outgoing arc for a CLOSED shape). A triple constraint
//! whose value expression refers to a shape (a reference or an inline shape) goes on to
//! the neighbourhoods of the values, at any depth and through recursion. So the result
//! of an association can change only if a changed triple is an arc read at its node or
//! at a node its references reach. Those nodes are found by walking the referring
//! constraints backwards from the changed triples' endpoints, in the state before the
//! write and in the state after it (the first changed arc on the way from the node is
//! reached over unchanged arcs, so it is reached in both).
//!
//! A `{FOCUS p o}` or `{s p FOCUS}` selector selects a node by an arc, so a changed `p`
//! arc can add or remove an association at its subject (or object). A term selector
//! selects its node in every state. A SPARQL selector can select anything, so a map
//! with one is validated in full.

use crate::ir::{Dir, Ir, Se, SeId};
use crate::{Association, NodeSelector, ShapeMap};
use oxrdf::Term;
use rustc_hash::FxHashSet;
use sparkles::id::Id;
use sparkles::store::Snapshot;
use sparkles::validation::DataGraph;

/// The static part of the analysis: what a validation reads and follows.
#[derive(Clone, Debug, Default)]
pub(super) struct Plan {
    /// arcs a validation follows from a node to a value whose neighbourhood it reads
    steps: Vec<(String, Dir)>,
    /// predicates of the outgoing arcs read at a node (`None`: every one)
    out: Option<FxHashSet<String>>,
    /// predicates of the incoming arcs read at a node
    inn: FxHashSet<String>,
    /// the predicates of `{FOCUS p o}` (`true`) and `{s p FOCUS}` (`false`) selectors
    selectors: Vec<(String, bool)>,
    /// the map has a SPARQL selector
    pub sparql: bool,
}

/// Whether a value expression reads the neighbourhood of the value.
fn refers(ir: &Ir, se: SeId, depth: usize) -> bool {
    if depth > 256 {
        return true;
    }
    match &ir.ses[se.index()] {
        Se::Ref(_) | Se::Shape(_) => true,
        Se::And(xs) | Se::Or(xs) => xs.iter().any(|x| refers(ir, *x, depth + 1)),
        Se::Not(x) => refers(ir, *x, depth + 1),
        Se::Nc(_) | Se::External => false,
    }
}

impl Plan {
    pub fn new(ir: &Ir, map: &ShapeMap) -> Plan {
        let mut p = Plan {
            out: Some(FxHashSet::default()),
            ..Default::default()
        };
        let mut steps = FxHashSet::default();
        for s in &ir.shapes {
            if s.closed {
                p.out = None;
            }
            for tc in &s.tcs {
                match tc.dir {
                    Dir::Out => {
                        if let Some(o) = &mut p.out {
                            o.insert(tc.pred.clone());
                        }
                    }
                    Dir::In => {
                        p.inn.insert(tc.pred.clone());
                    }
                }
                if tc.value.is_some_and(|v| refers(ir, v, 0))
                    && steps.insert((tc.pred.clone(), tc.dir))
                {
                    p.steps.push((tc.pred.clone(), tc.dir));
                }
            }
            if let Some(o) = &mut p.out {
                o.extend(s.extra.iter().cloned());
            }
        }
        for a in &map.0 {
            match &a.node {
                NodeSelector::Term(_) => {}
                NodeSelector::Focus {
                    predicate,
                    focus_is_subject,
                    ..
                } => p
                    .selectors
                    .push((predicate.as_str().to_string(), *focus_is_subject)),
                NodeSelector::Sparql(_) => p.sparql = true,
            }
        }
        p
    }

    /// The nodes whose associations the changed triples (`(s, p, o)`, ids of `view`)
    /// can affect, over the data graphs of the states before and after the write;
    /// `None` when the walk visits more than `max_visit` nodes.
    pub fn affected(
        &self,
        view: &Snapshot,
        states: [Option<&DataGraph>; 2],
        changes: &[[Id; 3]],
        max_visit: usize,
    ) -> sparkles::Result<Option<Vec<Id>>> {
        let ids = |ps: &mut dyn Iterator<Item = &String>| -> FxHashSet<Id> {
            ps.filter_map(|p| view.lookup_iri(p)).collect()
        };
        let out = self.out.as_ref().map(|o| ids(&mut o.iter()));
        let inn = ids(&mut self.inn.iter());
        let selectors: Vec<(Id, bool)> = self
            .selectors
            .iter()
            .filter_map(|(p, subj)| view.lookup_iri(p).map(|id| (id, *subj)))
            .collect();
        let steps: Vec<(Id, Dir)> = self
            .steps
            .iter()
            .filter_map(|(p, d)| view.lookup_iri(p).map(|id| (id, *d)))
            .collect();
        let mut seen: FxHashSet<Id> = FxHashSet::default();
        let mut stack = Vec::new();
        let add = |x: Id, seen: &mut FxHashSet<Id>, stack: &mut Vec<Id>| {
            if seen.insert(x) {
                stack.push(x);
            }
        };
        for &[s, p, o] in changes {
            if out.as_ref().is_none_or(|o| o.contains(&p)) {
                add(s, &mut seen, &mut stack);
            }
            if inn.contains(&p) {
                add(o, &mut seen, &mut stack);
            }
            for &(sp, subj) in &selectors {
                if sp == p {
                    add(if subj { s } else { o }, &mut seen, &mut stack);
                }
            }
        }
        // walk the referring constraints backwards
        while let Some(y) = stack.pop() {
            for &(p, d) in &steps {
                for data in states.iter().flatten() {
                    let from = match d {
                        // (x, p, y): x reads y's neighbourhood
                        Dir::Out => data.subjects(p, y)?,
                        // (y, p, x): an inverse constraint at x
                        Dir::In => data.objects(y, p)?,
                    };
                    for x in from {
                        add(x, &mut seen, &mut stack);
                    }
                }
                if seen.len() > max_visit {
                    return Ok(None);
                }
            }
        }
        if seen.len() > max_visit {
            return Ok(None);
        }
        let mut v: Vec<Id> = seen.into_iter().collect();
        v.sort_unstable();
        Ok(Some(v))
    }
}

/// The associations of `map` at `nodes` (ids and terms of the state after the write)
/// in the state `data` reads (`None`: no data graph).
pub(super) fn associations(
    map: &ShapeMap,
    view: &Snapshot,
    data: Option<&DataGraph>,
    nodes: &[(Id, Term)],
) -> sparkles::Result<ShapeMap> {
    let mut out = Vec::new();
    let mut seen: FxHashSet<(&Term, &crate::ShapeLabel)> = FxHashSet::default();
    let fixed = |t: &Option<Term>| -> Option<Option<Id>> {
        match t {
            None => Some(None),
            Some(t) => view.lookup_term(t).map(Some),
        }
    };
    for (id, term) in nodes {
        for a in &map.0 {
            // the node as the full expansion names it: a term selector's own term
            let mut node = term;
            let selects = match &a.node {
                NodeSelector::Term(t) => {
                    node = t;
                    t == term || view.lookup_term(t) == Some(*id)
                }
                NodeSelector::Focus {
                    subject,
                    predicate,
                    object,
                    focus_is_subject,
                } => {
                    let (Some(data), Some(p)) = (data, view.lookup_iri(predicate.as_str())) else {
                        continue;
                    };
                    if *focus_is_subject {
                        match fixed(object) {
                            None => false,
                            Some(Some(o)) => data.objects(*id, p)?.contains(&o),
                            Some(None) => !data.objects(*id, p)?.is_empty(),
                        }
                    } else {
                        match fixed(subject) {
                            None => false,
                            Some(Some(s)) => data.subjects(p, *id)?.contains(&s),
                            Some(None) => !data.subjects(p, *id)?.is_empty(),
                        }
                    }
                }
                // a map with a SPARQL selector is not validated incrementally
                NodeSelector::Sparql(_) => false,
            };
            if selects && seen.insert((node, &a.shape)) {
                out.push(Association {
                    node: NodeSelector::Term(node.clone()),
                    shape: a.shape.clone(),
                });
            }
        }
    }
    Ok(ShapeMap(out))
}
