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
//! That walk reaches every node that refers to a changed one, however little the write
//! changes. When the guard knows the typing of the head (every pair the validations of
//! the map discovered, with its value), it walks only where typings change: it types
//! the pairs of the nodes the write touched, reading the other pairs the head had as
//! `true` from that typing, and goes on to the nodes that refer to a pair whose value
//! changed. A pair the head had as `false` is typed again wherever it is read, because
//! a greatest fixed point can turn it `true`. So a new `foaf:knows` arc between two
//! people who conform validates one node, not every person who knows them.
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

    /// Whether a validation follows references from a node to its values (the case
    /// where a changed arc can change the results of other nodes).
    pub fn refers(&self) -> bool {
        !self.steps.is_empty()
    }

    /// The plan's predicates as ids of `view` (predicates it does not have are left
    /// out: no arc has them).
    pub fn resolve(&self, view: &Snapshot) -> Resolved {
        let ids = |ps: &mut dyn Iterator<Item = &String>| -> FxHashSet<Id> {
            ps.filter_map(|p| view.lookup_iri(p)).collect()
        };
        Resolved {
            out: self.out.as_ref().map(|o| ids(&mut o.iter())),
            inn: ids(&mut self.inn.iter()),
            selectors: self
                .selectors
                .iter()
                .filter_map(|(p, subj)| view.lookup_iri(p).map(|id| (id, *subj)))
                .collect(),
            steps: self
                .steps
                .iter()
                .filter_map(|(p, d)| view.lookup_iri(p).map(|id| (id, *d)))
                .collect(),
        }
    }

    /// The nodes whose neighbourhood a changed triple (`(s, p, o)`, ids of `view`) is
    /// part of, and those a changed selector arc may select or unselect.
    pub fn direct(r: &Resolved, changes: &[[Id; 3]]) -> Vec<Id> {
        let mut seen: FxHashSet<Id> = FxHashSet::default();
        let mut out = Vec::new();
        let mut add = |x: Id| {
            if seen.insert(x) {
                out.push(x);
            }
        };
        for &[s, p, o] in changes {
            if r.out.as_ref().is_none_or(|out| out.contains(&p)) {
                add(s);
            }
            if r.inn.contains(&p) {
                add(o);
            }
            for &(sp, subj) in &r.selectors {
                if sp == p {
                    add(if subj { s } else { o });
                }
            }
        }
        out
    }

    /// The nodes that read the neighbourhood of `y` through one referring constraint, in
    /// `data`.
    pub fn readers(r: &Resolved, data: &DataGraph, y: Id) -> sparkles::Result<Vec<Id>> {
        let mut out = Vec::new();
        for &(p, d) in &r.steps {
            out.extend(match d {
                // (x, p, y): x reads y's neighbourhood
                Dir::Out => data.subjects(p, y)?,
                // (y, p, x): an inverse constraint at x
                Dir::In => data.objects(y, p)?,
            });
        }
        Ok(out)
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
        let r = self.resolve(view);
        let mut stack = Plan::direct(&r, changes);
        let mut seen: FxHashSet<Id> = stack.iter().copied().collect();
        // walk the referring constraints backwards
        while let Some(y) = stack.pop() {
            for data in states.iter().flatten() {
                for x in Plan::readers(&r, data, y)? {
                    if seen.insert(x) {
                        stack.push(x);
                    }
                }
            }
            if seen.len() > max_visit {
                return Ok(None);
            }
        }
        let mut v: Vec<Id> = seen.into_iter().collect();
        v.sort_unstable();
        Ok(Some(v))
    }
}

/// A [`Plan`] with its predicates resolved against one state.
pub(super) struct Resolved {
    out: Option<FxHashSet<Id>>,
    inn: FxHashSet<Id>,
    selectors: Vec<(Id, bool)>,
    steps: Vec<(Id, Dir)>,
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
