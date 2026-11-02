//! Quad-level plans of form changes (spec U02 §3.2): the values of flat fields, atomic
//! changes of nested blank-node children (§2.3.1) and `rdf:List` values.
//!
//! A stored blank label names data, never authority. A change touches a stored blank
//! node only when it is a visible value of a field that declares `sh:node`, on a chain of
//! such anchors from the root, and a save checks those anchors again under the writer
//! lock. New children take labels that no stored node has, so a client's label can never
//! alias a stored node.

use super::*;
use crate::store::WriteTxn;
use oxrdf::{BlankNode, NamedOrBlankNode};
use std::collections::{HashMap, VecDeque};

pub(super) const RDF_FIRST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#first";
pub(super) const RDF_REST: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#rest";
pub(super) const RDF_NIL: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
/// The most members a list field lists, and a list change writes.
pub const MAX_LIST_ITEMS: usize = MAX_FIELD_VALUES;
/// The most quads that removing nested values deletes with them, over one change.
pub const MAX_SUBTREE_QUADS: usize = MAX_CHANGED_TERMS;
/// Labels of the blank nodes a change creates start with this, which no stored label does.
const FRESH: &str = "new";

/// A change to a nested blank-node child of a form (spec U02 §2.3.1), committed
/// atomically with the root's changes.
#[derive(Clone, Debug)]
pub struct NodeChange {
    /// The child: the stored label of a value of `field` on `parent`, as the nested model
    /// gives it, or a new label that the `add` of that field's change names. A label
    /// that the field's `add` names is always a new child, whatever it spells.
    pub node: BlankNode,
    /// the node the child hangs from: `None` for the root, else another entry's `node`
    pub parent: Option<BlankNode>,
    /// the id of the parent's field whose value the child is
    pub field: String,
    pub changes: Vec<FieldChange>,
    /// the values the form loaded, by changed field (an existing child only)
    pub expect: BTreeMap<String, Vec<Term>>,
}

/// One quad a change deletes or inserts.
#[derive(Clone, Debug)]
pub(super) struct Op {
    insert: bool,
    quad: Quad,
}

/// A changed field, the node whose field it is and how its values compare.
#[derive(Clone)]
struct Changed {
    focus: Term,
    node: Option<Term>,
    id: String,
    path: FieldPath,
    list: bool,
}

/// A visible edge from a parent to an existing nested node that a change relies on.
#[derive(Clone)]
struct Anchor {
    parent: Term,
    parent_node: Option<Term>,
    field: String,
    path: FieldPath,
    child: Term,
    /// the child's target graph as planned, which its type or its edge can move
    target: Option<String>,
}

/// The quads a change deletes and inserts, and what its save checks under the writer.
pub(super) struct Plan {
    /// deletions first, then insertions
    ops: Vec<Op>,
    changed: Vec<Changed>,
    anchors: Vec<Anchor>,
    /// the stored blank nodes whose values the change removes
    removed: Vec<Term>,
    /// the deletions other than the removed children's own quads
    edges: Vec<Quad>,
    /// the removed children's quads, as planned
    subtree: BTreeSet<String>,
    /// the nodes whose validation the change can affect (new nodes by their plan labels)
    pub(super) touched: Vec<Term>,
    /// distinct changed triples in graphs of the validation data graph
    pub(super) quads: Vec<Quad>,
    /// the nested nodes of the change, with the term the request names them by and their
    /// fields, for mapping check results
    pub(super) nodes: Vec<(Term, Term, FormLayout)>,
    /// nested entries whose node no longer hangs from its parent: answered as conflicts
    pub(super) lost: Vec<FieldConflict>,
}

impl Plan {
    /// The change as SPARQL Update text, for previews. Without nested values it is a
    /// `DELETE DATA` and `INSERT DATA` request; stored blank nodes appear by their labels
    /// and new ones by their plan labels, which `DELETE DATA` cannot run.
    pub(super) fn update(&self) -> String {
        let block = |insert: bool| {
            let mut by: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();
            for op in self.ops.iter().filter(|o| o.insert == insert) {
                let g = match &op.quad.graph_name {
                    GraphName::NamedNode(n) => Some(n.as_str().to_string()),
                    _ => None,
                };
                by.entry(g).or_default().push(format!(
                    "{} {} {}",
                    op.quad.subject, op.quad.predicate, op.quad.object
                ));
            }
            let mut s = String::new();
            for (g, lines) in &by {
                match g {
                    None => {
                        for l in lines {
                            s.push_str(&format!("  {l} .\n"));
                        }
                    }
                    Some(g) => {
                        s.push_str(&format!("  GRAPH <{g}> {{\n"));
                        for l in lines {
                            s.push_str(&format!("    {l} .\n"));
                        }
                        s.push_str("  }\n");
                    }
                }
            }
            s
        };
        let mut update = String::new();
        if self.ops.iter().any(|o| !o.insert) {
            update.push_str(&format!("DELETE DATA {{\n{}}}", block(false)));
        }
        if self.ops.iter().any(|o| o.insert) {
            if !update.is_empty() {
                update.push_str(" ;\n");
            }
            update.push_str(&format!("INSERT DATA {{\n{}}}", block(true)));
        }
        update
    }
}

/// Apply a plan to a write transaction: delete by the ids of stored terms, insert with
/// new ids for the plan's new blank nodes. The transaction checks the caller's graph
/// grants on each quad and its triple protections at the commit. Answers the ids the new
/// blank nodes took.
pub(super) fn apply(
    txn: &mut WriteTxn<'_>,
    plan: &Plan,
    limits: &Limits,
) -> Result<HashMap<String, Id>> {
    let mut labels: HashMap<String, Id> = HashMap::new();
    let lookup = |txn: &WriteTxn<'_>, t: &Term| -> Option<Id> {
        match t {
            Term::BlankNode(b) => crate::store::parse_bnode_label(b.as_str()),
            t => crate::id::inline_id(t).or_else(|| txn.lookup_key(&crate::id::term_key(t))),
        }
    };
    for (i, op) in plan.ops.iter().enumerate() {
        if i % 256 == 0 {
            limits.check()?;
        }
        let subject: Term = op.quad.subject.clone().into();
        let graph = match &op.quad.graph_name {
            GraphName::DefaultGraph => None,
            GraphName::NamedNode(n) => Some(Term::NamedNode(n.clone())),
            GraphName::BlankNode(b) => Some(Term::BlankNode(b.clone())),
        };
        if op.insert {
            for t in [&subject, &op.quad.object] {
                if let Term::BlankNode(b) = t
                    && !b.as_str().starts_with(FRESH)
                    && !labels.contains_key(b.as_str())
                {
                    let id = crate::store::parse_bnode_label(b.as_str()).ok_or_else(|| {
                        Error::invalid(format!("_:{} is not a stored blank node", b.as_str()))
                    })?;
                    labels.insert(b.as_str().to_string(), id);
                }
            }
            let ids = txn.encode_quad(&op.quad, &mut labels)?;
            txn.insert(ids)?;
        } else {
            let ids = (
                lookup(txn, &subject),
                txn.lookup_key(&crate::id::iri_key(op.quad.predicate.as_str())),
                lookup(txn, &op.quad.object),
                match &graph {
                    None => Some(Id::DEFAULT_GRAPH),
                    Some(g) => lookup(txn, g),
                },
            );
            if let (Some(s), Some(p), Some(o), Some(g)) = ids {
                txn.delete([s, p, o, g])?;
            }
        }
    }
    labels.retain(|k, _| k.starts_with(FRESH));
    Ok(labels)
}

/// A term with a plan's new blank node replaced by the stored node it became.
pub(super) fn stored(t: &Term, fresh: &HashMap<String, Id>) -> Term {
    match t {
        Term::BlankNode(b) => match fresh.get(b.as_str()) {
            Some(id) => Term::BlankNode(BlankNode::new_unchecked(crate::id::bnode_label(
                id.payload(),
            ))),
            None => t.clone(),
        },
        t => t.clone(),
    }
}

/// A quad with a plan's new blank nodes replaced by the stored nodes they became.
pub(super) fn stored_quad(q: &Quad, fresh: &HashMap<String, Id>) -> Quad {
    let subject = match stored(&q.subject.clone().into(), fresh) {
        Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
        Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
        _ => q.subject.clone(),
    };
    Quad::new(
        subject,
        q.predicate.clone(),
        stored(&q.object, fresh),
        q.graph_name.clone(),
    )
}

fn subject_of(t: &Term) -> Result<NamedOrBlankNode> {
    match t {
        Term::NamedNode(n) => Ok(NamedOrBlankNode::NamedNode(n.clone())),
        Term::BlankNode(b) => Ok(NamedOrBlankNode::BlankNode(b.clone())),
        t => Err(Error::invalid(format!(
            "{t} cannot be the subject of a value"
        ))),
    }
}

fn graph_name(g: Option<&str>) -> GraphName {
    g.map_or(GraphName::DefaultGraph, |g| {
        GraphName::NamedNode(NamedNode::new_unchecked(g))
    })
}

/// The quad of ids at `snap`, or `None` when a term is unknown.
fn quad_of(snap: &Snapshot, q: [Id; 4]) -> Option<Quad> {
    let subject = match snap.term(q[0])? {
        Term::NamedNode(n) => NamedOrBlankNode::NamedNode(n),
        Term::BlankNode(b) => NamedOrBlankNode::BlankNode(b),
        _ => return None,
    };
    let Term::NamedNode(predicate) = snap.term(q[1])? else {
        return None;
    };
    let graph = if q[3] == Id::DEFAULT_GRAPH {
        GraphName::DefaultGraph
    } else {
        match snap.term(q[3])? {
            Term::NamedNode(n) => GraphName::NamedNode(n),
            Term::BlankNode(b) => GraphName::BlankNode(b),
            _ => return None,
        }
    };
    Some(Quad::new(subject, predicate, snap.term(q[2])?, graph))
}

/// The quads that removing the stored blank nodes `roots` deletes with them: for each
/// node that no quad refers to once `deleted` are gone, in the state without the view's
/// mask, its outgoing quads the view sees, and so on through their blank objects. A node
/// that another quad still refers to is looked at again when a later deletion removes
/// one of its references. A seen quad in a graph the caller cannot write refuses the
/// change; hidden quads stay. A node with more references or quads than one scan reads
/// refuses the change, since its references cannot be ruled out.
fn subtree_at(
    raw: &Arc<Snapshot>,
    view: &Reader<'_>,
    write: Option<&GraphAccess>,
    roots: &[Term],
    deleted: &[Quad],
) -> Result<Vec<Quad>> {
    let mut gone: BTreeSet<String> = deleted.iter().map(|q| q.to_string()).collect();
    let mut out = Vec::new();
    let mut done = BTreeSet::new();
    let mut work: VecDeque<Term> = roots.iter().cloned().collect();
    let everything = Reader {
        snap: raw.clone(),
        access: None,
        masked: false,
        limits: view.limits.clone(),
    };
    let too_many = || {
        Error::invalid(format!(
            "removing nested values would read more than {MAX_SUBTREE_QUADS} quads of one node"
        ))
    };
    while let Some(node) = work.pop_front() {
        view.limits.unit()?;
        if done.contains(&node.to_string()) {
            continue;
        }
        let Some(id) = raw.lookup_term(&node) else {
            continue;
        };
        // another quad still refers to the node: only the removed edges go, and the
        // node is looked at again if a later deletion removes that reference
        let mut referenced = false;
        let refs = everything.scan(Perm::Osp, &[id.0], MAX_SUBTREE_QUADS + 1)?;
        for q in &refs {
            match quad_of(raw, *q) {
                Some(quad) if gone.contains(&quad.to_string()) => {}
                _ => {
                    referenced = true;
                    break;
                }
            }
        }
        if referenced {
            continue;
        }
        if refs.len() > MAX_SUBTREE_QUADS {
            return Err(too_many());
        }
        done.insert(node.to_string());
        let Some(vid) = view.snap.lookup_term(&node) else {
            continue;
        };
        let own = view.scan(Perm::Spo, &[vid.0], MAX_SUBTREE_QUADS + 1)?;
        if own.len() > MAX_SUBTREE_QUADS {
            return Err(too_many());
        }
        for q in own {
            let Some(graph) = view.graph(q[3])? else {
                continue;
            };
            if !writable_in(write, graph.as_deref()) {
                return Err(Error::NotPermitted(format!(
                    "removing the nested value {node} would delete a quad in {}, which you \
                     cannot write",
                    graph.map_or("the default graph".into(), |g| format!("<{g}>"))
                )));
            }
            let Some(quad) = quad_of(&view.snap, q) else {
                continue;
            };
            if gone.insert(quad.to_string()) {
                if let Term::BlankNode(_) = &quad.object {
                    work.push_back(quad.object.clone());
                }
                out.push(quad);
                if out.len() > MAX_SUBTREE_QUADS {
                    return Err(Error::invalid(format!(
                        "removing nested values would delete more than {MAX_SUBTREE_QUADS} quads"
                    )));
                }
            }
        }
    }
    Ok(out)
}

/// A node of a change: the root, or a nested entry resolved against the planned state.
struct Node<'b> {
    term: Term,
    built: Owner<'b>,
    new: bool,
    /// the entry's label (`None`: the root)
    label: Option<BlankNode>,
    /// the entry's index in `FormChange::nested`
    entry: Option<usize>,
}

enum Owner<'b> {
    Root(&'b Built),
    Child(Box<Built>),
}

impl std::ops::Deref for Owner<'_> {
    type Target = Built;
    fn deref(&self) -> &Built {
        match self {
            Owner::Root(b) => b,
            Owner::Child(b) => b,
        }
    }
}

/// What planning collects over the nodes of a change.
#[derive(Default)]
struct Acc {
    del: Vec<Quad>,
    ins: Vec<Quad>,
    quads: Vec<Quad>,
    touched: Vec<Term>,
    changed: Vec<Changed>,
    removed: Vec<Term>,
    fresh: usize,
}

impl Acc {
    fn fresh(&mut self) -> Term {
        let t = Term::BlankNode(BlankNode::new_unchecked(format!("{FRESH}{}", self.fresh)));
        self.fresh += 1;
        t
    }
}

/// The number of terms a change names, over its fields, lists and nested entries.
fn change_terms(change: &FormChange) -> usize {
    let field =
        |c: &FieldChange| c.add.len() + c.remove.len() + c.list.as_ref().map_or(0, Vec::len);
    change.changes.iter().map(field).sum::<usize>()
        + change
            .nested
            .iter()
            .map(|n| 1 + n.changes.iter().map(field).sum::<usize>())
            .sum::<usize>()
}

impl Built {
    /// The plan of a change, after checking it against the fields of every node it names.
    pub(super) fn plan(&self, change: &FormChange, track: bool) -> Result<Plan> {
        let terms = change_terms(change);
        if terms > MAX_CHANGED_TERMS {
            return Err(Error::invalid(format!(
                "a change may remove and add at most {MAX_CHANGED_TERMS} terms, not {terms}"
            )));
        }
        let read = self.read()?;
        let mut acc = Acc::default();
        if let Some(c) = &self.new_class {
            // a new instance takes an IRI nothing describes yet
            if describes(&read, &self.focus)? {
                return Err(used_iri(&self.focus));
            }
            if !self.writable_graph(self.target.as_deref()) {
                return Err(Error::NotPermitted(format!(
                    "new values go to {}, which you cannot write",
                    self.target
                        .as_deref()
                        .map_or("the default graph".into(), |g| format!("<{g}>"))
                )));
            }
            let rdf_type = NamedNode::new_unchecked(RDF_TYPE);
            let ty = Term::NamedNode(c.clone());
            acc.ins.push(Quad::new(
                subject_of(&self.focus)?,
                rdf_type.clone(),
                ty.clone(),
                graph_name(self.target.as_deref()),
            ));
            if track {
                self.change_quad(
                    &mut acc.quads,
                    &self.focus,
                    &rdf_type,
                    &ty,
                    self.target.as_deref(),
                )?;
            }
        }
        drop(read);
        let (nodes, anchors, lost) = self.resolve(change)?;
        acc.touched.push(self.focus.clone());
        for i in 0..nodes.len() {
            let n = &nodes[i];
            if i > 0 {
                acc.touched.push(n.term.clone());
            }
            let changes = match n.entry {
                None => &change.changes,
                Some(e) => &change.nested[e].changes,
            };
            n.built
                .plan_fields(changes, i, &nodes, change, track, &mut acc)?;
        }
        // the removed children's own quads, when nothing else refers to them
        let mut edges = acc.del.clone();
        let subtree = if acc.removed.is_empty() {
            Vec::new()
        } else {
            let view = self.read()?;
            subtree_at(
                &self.raw,
                &view,
                self.write_access.as_deref(),
                &acc.removed,
                &acc.del,
            )?
        };
        for q in &subtree {
            let subject: Term = q.subject.clone().into();
            if nodes.iter().skip(1).any(|n| !n.new && n.term == subject) {
                return Err(Error::invalid(format!(
                    "the change removes the nested node {subject} and also changes it"
                )));
            }
            if track {
                let g = match &q.graph_name {
                    GraphName::NamedNode(n) => Some(n.as_str()),
                    _ => None,
                };
                self.change_quad(&mut acc.quads, &subject, &q.predicate, &q.object, g)?;
            }
        }
        let subtree_keys = subtree.iter().map(|q| q.to_string()).collect();
        acc.del.extend(subtree);
        // a change whose only nodes are lost answers their conflicts instead
        if acc.del.is_empty() && acc.ins.is_empty() && lost.is_empty() {
            return Err(Error::invalid("the change changes nothing"));
        }
        if acc.del.len() + acc.ins.len() > MAX_CHANGED_TERMS + MAX_SUBTREE_QUADS {
            return Err(Error::invalid("the change writes too many quads"));
        }
        let mut seen = BTreeSet::new();
        acc.touched.retain(|t| seen.insert(t.to_string()));
        edges.dedup_by(|a, b| a == b);
        Ok(Plan {
            ops: acc
                .del
                .into_iter()
                .map(|quad| Op {
                    insert: false,
                    quad,
                })
                .chain(acc.ins.into_iter().map(|quad| Op { insert: true, quad }))
                .collect(),
            changed: acc.changed,
            anchors,
            removed: acc.removed,
            edges,
            subtree: subtree_keys,
            touched: acc.touched,
            quads: acc.quads,
            nodes: nodes
                .iter()
                .skip(1)
                .map(|n| {
                    let named = n
                        .label
                        .clone()
                        .map_or_else(|| n.term.clone(), Term::BlankNode);
                    (n.term.clone(), named, n.built.layout.clone())
                })
                .collect(),
            lost,
        })
    }

    /// The nested entries of a change, resolved in parent order from the root: a new
    /// child that its parent field's `add` names, or a pooled child that is a visible
    /// value of that field now. An entry whose child no longer hangs from its parent is
    /// lost, with the entries below it, and answered as a conflict on the parent's field.
    #[allow(clippy::type_complexity)]
    fn resolve(
        &self,
        change: &FormChange,
    ) -> Result<(Vec<Node<'_>>, Vec<Anchor>, Vec<FieldConflict>)> {
        let mut nodes = vec![Node {
            term: self.focus.clone(),
            built: Owner::Root(self),
            new: false,
            label: None,
            entry: None,
        }];
        let mut anchors = Vec::new();
        let mut lost: Vec<FieldConflict> = Vec::new();
        let mut lost_labels: BTreeSet<String> = BTreeSet::new();
        if change.nested.is_empty() {
            return Ok((nodes, anchors, lost));
        }
        let mut seen = BTreeSet::new();
        for e in &change.nested {
            if !seen.insert(e.node.as_str()) {
                return Err(Error::invalid(format!(
                    "nested: _:{} has two entries",
                    e.node.as_str()
                )));
            }
        }
        let mut pool: Option<nested::Pool> = None;
        let mut pending: Vec<usize> = (0..change.nested.len()).collect();
        while !pending.is_empty() {
            let mut next = Vec::new();
            let before = pending.len();
            for i in pending {
                self.limits.unit()?;
                let e = &change.nested[i];
                if let Some(p) = &e.parent
                    && lost_labels.contains(p.as_str())
                {
                    lost_labels.insert(e.node.as_str().to_string());
                    continue;
                }
                let Some(pi) = (match &e.parent {
                    None => Some(0),
                    Some(p) => nodes.iter().position(|n| n.label.as_ref() == Some(p)),
                }) else {
                    next.push(i);
                    continue;
                };
                let parent = &nodes[pi];
                let f = parent
                    .built
                    .layout
                    .fields
                    .iter()
                    .find(|f| f.id() == e.field)
                    .filter(|f| {
                        let t = parent
                            .built
                            .target
                            .as_ref()
                            .map(|g| Term::NamedNode(NamedNode::new_unchecked(g.as_str())));
                        !parent.built.hidden(f, t.as_ref())
                    })
                    .ok_or_else(|| {
                        Error::invalid(format!("nested: no field {} on {}", e.field, parent.term))
                    })?;
                if f.constraints.node.is_empty() || f.path.predicate().is_none() {
                    return Err(Error::invalid(format!(
                        "nested: field {} declares no sh:node on a predicate or inverse path, \
                         so its values are not nested forms",
                        e.field
                    )));
                }
                let child_term = Term::BlankNode(e.node.clone());
                let parent_changes = match parent.entry {
                    None => &change.changes,
                    Some(pe) => &change.nested[pe].changes,
                };
                let is_new = parent_changes
                    .iter()
                    .any(|c| c.field == e.field && c.add.contains(&child_term));
                if is_new {
                    let shapes: Vec<usize> = f
                        .constraints
                        .node
                        .iter()
                        .filter_map(|t| self.shapes.shape_id(t))
                        .collect();
                    let term = Term::BlankNode(BlankNode::new_unchecked(format!(
                        "{FRESH}c{}",
                        nodes.len()
                    )));
                    let built =
                        parent
                            .built
                            .fresh_child(&term, &shapes, parent.built.target.clone())?;
                    nodes.push(Node {
                        term,
                        built: Owner::Child(Box::new(built)),
                        new: true,
                        label: Some(e.node.clone()),
                        entry: Some(i),
                    });
                    continue;
                }
                if parent.new {
                    return Err(Error::invalid(format!(
                        "nested: _:{} hangs from a new node, so it must be new too: name it in \
                         the add of field {}",
                        e.node.as_str(),
                        e.field
                    )));
                }
                // the child must be a visible value of the parent's field now
                let values = parent.built.read()?.values(&parent.term, &f.path)?;
                let anchor_graph = values
                    .iter()
                    .find(|v| v.term == child_term)
                    .map(|v| v.graph.clone());
                let Some(anchor_graph) = anchor_graph else {
                    lost_labels.insert(e.node.as_str().to_string());
                    if !lost.iter().any(|c| {
                        c.id == e.field && c.node == parent.label.clone().map(Term::BlankNode)
                    }) {
                        lost.push(FieldConflict {
                            id: e.field.clone(),
                            node: parent.label.clone().map(Term::BlankNode),
                            now: values.into_iter().map(|v| v.term).collect(),
                        });
                    }
                    continue;
                };
                if pool.is_none() {
                    pool = Some(self.pool(true, "")?.1);
                }
                let pooled = pool
                    .as_ref()
                    .and_then(|p| p.get(&child_term))
                    .ok_or_else(|| {
                        Error::invalid(format!(
                            "nested: _:{} is not a nested node of this form",
                            e.node.as_str()
                        ))
                    })?;
                let shapes = pooled.shapes.to_vec();
                let built = self.child(&child_term, &shapes, Some(anchor_graph))?;
                anchors.push(Anchor {
                    parent: parent.term.clone(),
                    parent_node: parent.label.clone().map(Term::BlankNode),
                    field: e.field.clone(),
                    path: f.path.clone(),
                    child: child_term.clone(),
                    target: built.target.clone(),
                });
                nodes.push(Node {
                    term: child_term,
                    built: Owner::Child(Box::new(built)),
                    new: false,
                    label: Some(e.node.clone()),
                    entry: Some(i),
                });
            }
            if next.len() == before {
                let e = &change.nested[next[0]];
                return Err(Error::invalid(format!(
                    "nested: the parent of _:{} is neither the root nor another entry",
                    e.node.as_str()
                )));
            }
            pending = next;
        }
        Ok((nodes, anchors, lost))
    }

    /// Plan the field changes of one node of a change into `acc`.
    fn plan_fields(
        &self,
        changes: &[FieldChange],
        me: usize,
        nodes: &[Node<'_>],
        change: &FormChange,
        track: bool,
        acc: &mut Acc,
    ) -> Result<()> {
        let read = self.read()?;
        let node = &nodes[me];
        let focus = &self.focus;
        let target = self
            .target
            .as_ref()
            .map(|g| Term::NamedNode(NamedNode::new_unchecked(g.as_str())));
        let target_name = || {
            self.target
                .as_deref()
                .map_or("the default graph".into(), |g| format!("<{g}>"))
        };
        let on = match &node.label {
            None => String::new(),
            Some(l) => format!(" of _:{}", l.as_str()),
        };
        for c in changes {
            let f = self
                .layout
                .fields
                .iter()
                .find(|f| f.id() == c.field)
                .filter(|f| !self.hidden(f, target.as_ref()))
                .ok_or_else(|| Error::invalid(format!("no field {}{on} in this form", c.field)))?;
            if c.add.is_empty() && c.remove.is_empty() && c.list.is_none() {
                continue;
            }
            if f.read_only {
                return Err(Error::NotPermitted(format!(
                    "field {}{on} is read-only (dash:readOnly)",
                    c.field
                )));
            }
            if matches!(f.path, FieldPath::Complex(_)) {
                return Err(Error::invalid(format!(
                    "field {}{on} has a complex path, which a form cannot change",
                    c.field
                )));
            }
            if self.protected(f, target.as_ref()) {
                return Err(Error::NotPermitted(format!(
                    "your access protects the predicate of field {}{on} from writes",
                    c.field
                )));
            }
            let p = f.path.predicate().expect("a predicate or inverse path");
            let inverse = matches!(f.path, FieldPath::Inverse(_));
            let nested_field = !f.constraints.node.is_empty();
            let is_list = f.constraints.list.is_some();
            if is_list != c.list.is_some() {
                return Err(Error::invalid(if is_list {
                    format!("field {}{on} is a list: change it with list", c.field)
                } else {
                    format!("field {}{on} is not a list field", c.field)
                }));
            }
            if is_list && (inverse || !c.add.is_empty() || !c.remove.is_empty()) {
                return Err(Error::invalid(format!(
                    "field {}{on}: a list change names its members in list alone",
                    c.field
                )));
            }
            // the quad of one value, with the value as object, or as subject on an inverse path
            let edge = |v: &Term, graph: Option<&str>| -> Result<Quad> {
                if inverse {
                    let Term::NamedNode(s) = v else {
                        return Err(Error::invalid(format!(
                            "field {}{on} has an inverse path: its values are IRIs",
                            c.field
                        )));
                    };
                    Ok(Quad::new(
                        s.clone(),
                        p.clone(),
                        focus.clone(),
                        graph_name(graph),
                    ))
                } else {
                    Ok(Quad::new(
                        subject_of(focus)?,
                        p.clone(),
                        v.clone(),
                        graph_name(graph),
                    ))
                }
            };
            let stored = read.values(focus, &f.path)?;
            let remove = |acc: &mut Acc, v: &Term| -> Result<()> {
                let graphs: Vec<Option<String>> = stored
                    .iter()
                    .filter(|s| s.term == *v)
                    .map(|s| s.graph.clone())
                    .collect();
                for g in &graphs {
                    if !self.writable_graph(g.as_deref()) {
                        return Err(Error::NotPermitted(format!(
                            "field {}{on}: the value {v} is in a graph you cannot write",
                            c.field
                        )));
                    }
                    let q = edge(v, g.as_deref())?;
                    if track {
                        self.change_quad(
                            &mut acc.quads,
                            &q.subject.clone().into(),
                            p,
                            &q.object,
                            g.as_deref(),
                        )?;
                    }
                    acc.del.push(q);
                }
                match v {
                    Term::NamedNode(_) => acc.touched.push(v.clone()),
                    Term::BlankNode(_) if !graphs.is_empty() => acc.removed.push(v.clone()),
                    _ => {}
                }
                Ok(())
            };
            for v in &c.remove {
                if let Term::BlankNode(b) = v {
                    if !nested_field || inverse {
                        return Err(Error::invalid(format!(
                            "field {}{on}: a blank node cannot be named in a change",
                            c.field
                        )));
                    }
                    if nodes.iter().any(|n| !n.new && n.label.as_ref() == Some(b)) {
                        return Err(Error::invalid(format!(
                            "the change removes the nested node _:{} and also changes it",
                            b.as_str()
                        )));
                    }
                }
                remove(acc, v)?;
            }
            let adds = !c.add.is_empty() || c.list.is_some();
            if adds && !self.writable_graph(self.target.as_deref()) {
                return Err(Error::NotPermitted(format!(
                    "new values go to {}, which you cannot write",
                    target_name()
                )));
            }
            let insert = |acc: &mut Acc, s: &Term, pred: &NamedNode, o: &Term| -> Result<()> {
                let q = Quad::new(
                    subject_of(s)?,
                    pred.clone(),
                    o.clone(),
                    graph_name(self.target.as_deref()),
                );
                if track {
                    self.change_quad(&mut acc.quads, s, pred, o, self.target.as_deref())?;
                }
                acc.ins.push(q);
                Ok(())
            };
            if let Some(items) = &c.list {
                if items.len() > MAX_LIST_ITEMS {
                    return Err(Error::invalid(format!(
                        "field {}{on}: a list takes at most {MAX_LIST_ITEMS} members",
                        c.field
                    )));
                }
                // a field edits one well-formed list, as the model offers it
                if stored.len() > 1 || {
                    let mut bad = false;
                    for v in &stored {
                        bad |= read.list_items(&v.term)?.is_none();
                    }
                    bad
                } {
                    return Err(Error::NotPermitted(format!(
                        "field {}{on} does not hold one well-formed list, so it is read-only",
                        c.field
                    )));
                }
                // the stored list goes, with its cells
                let old: Vec<Term> = stored.iter().map(|v| v.term.clone()).collect();
                let mut done = BTreeSet::new();
                for v in &old {
                    if done.insert(v.to_string()) {
                        remove(acc, v)?;
                    }
                }
                let first = NamedNode::new_unchecked(RDF_FIRST);
                let rest = NamedNode::new_unchecked(RDF_REST);
                let nil = Term::NamedNode(NamedNode::new_unchecked(RDF_NIL));
                let cells: Vec<Term> = items.iter().map(|_| acc.fresh()).collect();
                insert(acc, focus, p, cells.first().unwrap_or(&nil))?;
                for (k, item) in items.iter().enumerate() {
                    if !matches!(item, Term::NamedNode(_) | Term::Literal(_)) {
                        return Err(Error::invalid(format!(
                            "field {}{on}: list members are IRIs or literals",
                            c.field
                        )));
                    }
                    insert(acc, &cells[k], &first, item)?;
                    insert(acc, &cells[k], &rest, cells.get(k + 1).unwrap_or(&nil))?;
                    if let Term::NamedNode(_) = item {
                        acc.touched.push(item.clone());
                    }
                }
            }
            for v in &c.add {
                let value = match v {
                    Term::BlankNode(b) => {
                        // a new child that a nested entry of this field describes
                        let child = nodes.iter().find(|n| {
                            n.new
                                && n.label.as_ref() == Some(b)
                                && n.entry.is_some_and(|e| {
                                    change.nested[e].field == c.field
                                        && change.nested[e].parent == node.label
                                })
                        });
                        match child {
                            Some(n) if nested_field && !inverse => n.term.clone(),
                            _ => {
                                return Err(Error::invalid(format!(
                                    "field {}{on}: the blank node _:{} in add must be a new \
                                     nested node that nested describes under this field",
                                    c.field,
                                    b.as_str()
                                )));
                            }
                        }
                    }
                    v => v.clone(),
                };
                let q = edge(&value, self.target.as_deref())?;
                if track {
                    self.change_quad(
                        &mut acc.quads,
                        &q.subject.clone().into(),
                        p,
                        &q.object,
                        self.target.as_deref(),
                    )?;
                }
                acc.ins.push(q);
                if let Term::NamedNode(_) = v {
                    acc.touched.push(v.clone());
                }
            }
            if !acc
                .changed
                .iter()
                .any(|x| x.id == c.field && x.focus == *focus)
                && !node.new
            {
                acc.changed.push(Changed {
                    focus: focus.clone(),
                    node: node.label.clone().map(Term::BlankNode),
                    id: c.field.clone(),
                    path: f.path.clone(),
                    list: is_list,
                });
            }
        }
        Ok(())
    }

    /// Add a changed triple to the validation's change set when it lies in a graph of
    /// the validation's data graph.
    pub(super) fn change_quad(
        &self,
        out: &mut Vec<Quad>,
        subject: &Term,
        predicate: &NamedNode,
        object: &Term,
        graph: Option<&str>,
    ) -> Result<()> {
        if graph.is_some_and(|g| self.vopts.exclude_graphs.iter().any(|x| x == g)) {
            return Ok(());
        }
        let selected = match self.vopts.data_graph.as_deref() {
            None => self.snap.union_default_graph || graph.is_none(),
            Some("urn:x-arq:UnionGraph") => true,
            Some("urn:x-arq:DefaultGraph") => graph.is_none(),
            Some(g) => graph == Some(g),
        } || graph.is_some_and(|g| self.vopts.extra_graphs.iter().any(|x| x == g));
        if !selected {
            return Ok(());
        }
        let subject = subject_of(subject)?;
        // A triple present in several selected graphs is one validation dependency.
        // Compare before cloning values so graph multiplicity cannot multiply retained terms.
        if out
            .iter()
            .any(|q| q.subject == subject && q.predicate == *predicate && q.object == *object)
        {
            return Ok(());
        }
        if out.len() > MAX_CHANGED_TERMS + MAX_SUBTREE_QUADS {
            return Err(Error::invalid(
                "form validation change admission exceeded its bound",
            ));
        }
        out.push(Quad::new(
            subject,
            predicate.clone(),
            object.clone(),
            graph_name(graph),
        ));
        Ok(())
    }
}

/// A changed field's values as the form loaded them, and their placement in the state
/// its plan was made on.
struct Check {
    changed: Changed,
    /// the loaded values' keys, sorted for a set and in order for a list
    want: Vec<String>,
    planned: BTreeSet<(String, Option<String>)>,
}

fn placed(values: &[FormValue]) -> BTreeSet<(String, Option<String>)> {
    values
        .iter()
        .map(|v| (v.term.to_string(), v.graph.clone()))
        .collect()
}

/// The keys a field's values compare by: the distinct terms of a field, or the members
/// of a list field's lists in order.
fn keys(read: &Reader<'_>, values: &[FormValue], list: bool) -> Result<(Vec<String>, Vec<Term>)> {
    if !list {
        let s = set(&mut values.iter().map(|v| v.term.clone()));
        return Ok((s.keys().cloned().collect(), s.into_values().collect()));
    }
    let mut terms = Vec::new();
    let mut heads = BTreeSet::new();
    for v in values {
        if !heads.insert(v.term.to_string()) {
            continue;
        }
        match read.list_items(&v.term)? {
            Some(items) => terms.extend(items.into_iter().map(|i| i.term)),
            // not a list now: compare by the value itself, which no member list equals
            None => terms.push(v.term.clone()),
        }
    }
    Ok((terms.iter().map(|t| t.to_string()).collect(), terms))
}

/// What a save checks again under the writer lock.
pub(super) struct Guard {
    focus: Term,
    new_instance: bool,
    checks: Vec<Check>,
    anchors: Vec<Anchor>,
    removed: Vec<Term>,
    edges: Vec<Quad>,
    subtree: BTreeSet<String>,
    write: Option<Arc<GraphAccess>>,
    policy: String,
    /// the root's target graph as planned
    target: Option<String>,
}

impl Guard {
    /// The checks of a plan: `change.expect` (or a nested entry's `expect`) must name
    /// every changed field of an existing node.
    pub(super) fn of(built: &Built, plan: &Plan, change: &FormChange) -> Result<Guard> {
        let read = built.read()?;
        let mut checks = Vec::new();
        for c in &plan.changed {
            let expect = match &c.node {
                None => &change.expect,
                Some(Term::BlankNode(b)) => change
                    .nested
                    .iter()
                    .find(|n| n.node == *b)
                    .map(|n| &n.expect)
                    .ok_or_else(|| Error::invalid("nested: an entry went missing"))?,
                Some(_) => unreachable!("nested nodes are blank"),
            };
            let on = match &c.node {
                None => String::new(),
                Some(n) => format!(" of {n}"),
            };
            let want = expect.get(&c.id).ok_or_else(|| {
                Error::invalid(format!(
                    "expect: give the values the form loaded for field {}{on}",
                    c.id
                ))
            })?;
            let want = if c.list {
                want.iter().map(|t| t.to_string()).collect()
            } else {
                set(&mut want.iter().cloned()).into_keys().collect()
            };
            checks.push(Check {
                changed: c.clone(),
                want,
                planned: placed(&read.values(&c.focus, &c.path)?),
            });
        }
        Ok(Guard {
            focus: built.focus.clone(),
            new_instance: built.new_class.is_some(),
            checks,
            anchors: plan.anchors.clone(),
            removed: plan.removed.clone(),
            edges: plan.edges.clone(),
            subtree: plan.subtree.clone(),
            write: built.write_access.clone(),
            policy: built.target_policy.clone(),
            target: built.target.clone(),
        })
    }

    /// The changed fields whose values at `snap`, under the caller's view, are not those
    /// the form loaded (with the parent fields of nested nodes that no longer hang from
    /// them); and whether, with the same values, the plan no longer fits the state:
    /// values in other graphs, another target graph or other quads under a removed child.
    pub(super) fn differ(
        &self,
        snap: &Arc<Snapshot>,
        access: Option<&GraphAccess>,
        limits: &Limits,
    ) -> Result<(Vec<FieldConflict>, bool)> {
        let mut read = Reader::new(snap, access)?;
        read.limits = limits.clone();
        // a new instance's IRI must still describe nothing when the write starts
        if self.new_instance && describes(&read, &self.focus)? {
            return Err(used_iri(&self.focus));
        }
        let mut out: Vec<FieldConflict> = Vec::new();
        let mut stale = false;
        for a in &self.anchors {
            let values = read.values(&a.parent, &a.path)?;
            // a child's new values go to a graph that its type or its edge decides
            if let Some(edge) = values.iter().find(|v| v.term == a.child) {
                let (target, _) = super::nested::child_target(
                    &read,
                    &a.child,
                    &self.policy,
                    Some(edge.graph.clone()),
                )?;
                stale |= target != a.target;
            }
            if !values.iter().any(|v| v.term == a.child)
                && !out
                    .iter()
                    .any(|c| c.id == a.field && c.node == a.parent_node)
            {
                out.push(FieldConflict {
                    id: a.field.clone(),
                    node: a.parent_node.clone(),
                    now: set(&mut values.into_iter().map(|v| v.term))
                        .into_values()
                        .collect(),
                });
            }
        }
        for c in &self.checks {
            let values = read.values(&c.changed.focus, &c.changed.path)?;
            let (now, terms) = keys(&read, &values, c.changed.list)?;
            if now != c.want {
                out.push(FieldConflict {
                    id: c.changed.id.clone(),
                    node: c.changed.node.clone(),
                    now: terms,
                });
            } else if placed(&values) != c.planned {
                stale = true;
            }
        }
        if out.is_empty() && !stale {
            // new values go to a graph that depends on the state
            stale = target_graph(&read, &self.focus, &self.policy)? != self.target;
        }
        if out.is_empty() && !stale && !self.removed.is_empty() {
            let now = subtree_at(
                snap,
                &read,
                self.write.as_deref(),
                &self.removed,
                &self.edges,
            )?;
            stale = now.iter().map(|q| q.to_string()).collect::<BTreeSet<_>>() != self.subtree;
        }
        Ok((out, stale))
    }
}
