//! Explanations of what RDFS on read answers (spec U10 §3.3).
//!
//! RDFS on read derives a triple in one step from a stored triple of the same graph and
//! the schema, so a tree has one data premise and the schema facts it used. The rules are
//! those of the W3C RDF 1.1 Semantics that Jena's `MatchRDFS` applies: `rdfs9` for a type
//! from a stored type and the closed class hierarchy, `rdfs2` and `rdfs3` for a type from
//! the domain or range its declaring property has, and `rdfs7` for a triple from a stored
//! triple of a subproperty. A subclass or subproperty fact the schema graph does not hold
//! directly is derived by `rdfs11` or `rdfs5` from a chain of facts it holds.

use super::{ExplainWorkspace, RdfsOnRead, RdfsSchema};
use crate::error::Result;
use crate::explain::{
    Derivation, ExplainBudgetExceeded, ExplainLimits, Explained, RuleRef, finish, term_bytes,
};
use crate::id::Id;
use crate::index::Perm;
use crate::store::Snapshot;
use oxrdf::vocab::{rdf, rdfs};
use oxrdf::{NamedNode, NamedNodeRef, Term};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

/// Where the schema's facts are, for the leaves of an explanation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaLeaves {
    /// a schema given as a document: its closed triples are leaves without a graph
    Document,
    /// a graph of the dataset the caller reads
    Graph(Id),
    /// a graph the caller does not read: no derivation that needs the schema is shown
    Hidden,
}

/// The most index reads one explanation makes.
const MAX_READS: usize = 100_000;

/// Resolve a stored leaf before the explainer chooses a derivation.
pub type AssertedResolver<'a> = dyn Fn(Id, &[Term; 3]) -> Result<Option<Derivation>> + 'a;

/// Resolve a stored leaf using the proof search's shared [`ExplainWorkspace`].
pub type WorkspaceResolver<'a> =
    dyn Fn(Id, &[Term; 3], &ExplainWorkspace) -> Result<Option<Derivation>> + 'a;

fn named_node_bytes(n: &NamedNode) -> usize {
    n.as_str()
        .len()
        .saturating_add(std::mem::size_of::<NamedNode>())
}

fn triple_copy_bytes(s: &Term, p: &NamedNode, o: &Term) -> usize {
    term_bytes(s)
        .saturating_add(named_node_bytes(p))
        .saturating_add(term_bytes(o))
}

fn triple_bytes(triple: &[Term; 3]) -> usize {
    triple
        .iter()
        .map(term_bytes)
        .fold(0usize, usize::saturating_add)
}

struct Cx<'a> {
    schema: Option<&'a RdfsSchema>,
    snap: &'a Snapshot,
    leaves: SchemaLeaves,
    deadline: Option<Instant>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    workspace: ExplainWorkspace,
    reads: std::cell::Cell<usize>,
    resolve: Option<&'a WorkspaceResolver<'a>>,
}

fn nn(n: NamedNodeRef<'_>) -> Term {
    Term::NamedNode(n.into_owned())
}

fn rule(name: &str) -> RuleRef {
    RuleRef {
        name: name.to_string(),
        profile: "rdfs-on-read".to_string(),
        owl2rl: None,
    }
}

impl Cx<'_> {
    /// Fail when the request was cancelled or passed its deadline.
    fn check(&self) -> Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
        {
            return Err(crate::error::Error::Cancelled);
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(budget("the explanation passed its timeout".to_string()));
        }
        Ok(())
    }

    /// Count one index read against the limit, and check the request's controls.
    fn read(&self) -> Result<()> {
        let n = self.reads.get() + 1;
        self.reads.set(n);
        if n > MAX_READS {
            return Err(budget(format!(
                "the explanation made its limit of {MAX_READS} index reads"
            )));
        }
        self.check()
    }

    /// Visit the rows of `prefix` in `perm` as they stream from the index, until `f`
    /// answers `false` or fails.
    fn scan_rows(
        &self,
        perm: Perm,
        prefix: &[u64],
        mut f: impl FnMut(&crate::index::Key) -> Result<bool>,
    ) -> Result<()> {
        let mut failed = None;
        self.snap.scan(perm, prefix, |c| {
            let (lo, hi) = match &c {
                crate::store::Chunk::Block(_, st, end) => (*st, *end),
                crate::store::Chunk::Row(_) => (0, 1),
            };
            for i in lo..hi {
                let k = match &c {
                    crate::store::Chunk::Block(b, _, _) => {
                        [b.cols[0][i], b.cols[1][i], b.cols[2][i], b.cols[3][i]]
                    }
                    crate::store::Chunk::Row(k) => *k,
                };
                match f(&k) {
                    Ok(true) => {}
                    Ok(false) => return Ok(false),
                    Err(e) => {
                        failed = Some(e);
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        })?;
        match failed {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn id(&self, t: &Term) -> Option<Id> {
        self.snap.lookup_term(t)
    }

    fn decode_term(&self, id: Id) -> Result<Option<Term>> {
        self.workspace
            .decode_term(self.snap, id, &mut || self.check())
    }

    fn schema(&self) -> &RdfsSchema {
        self.schema
            .expect("schema is loaded before rule search starts")
    }

    fn graph_name(&self, g: Id) -> Result<Option<String>> {
        self.workspace
            .graph_name(self.snap, g, &mut || self.check())
    }

    fn leaf(&self, g: Id, triple: &[Term; 3]) -> Result<Option<Derivation>> {
        self.check()?;
        match self.resolve {
            Some(resolve) => resolve(g, triple, &self.workspace),
            None => self
                .workspace
                .asserted_leaf(self.snap, g, triple, &mut || self.check())
                .map(Some),
        }
    }

    /// Build a leaf derivation by moving an already-owned, workspace-held candidate triple
    /// directly into the proof after decoding its graph name (avoiding a second copy of
    /// `triple` while the candidate is live).
    fn leaf_owned(&self, g: Id, triple: [Term; 3]) -> Result<Option<Derivation>> {
        self.check()?;
        match self.resolve {
            Some(resolve) => resolve(g, &triple, &self.workspace),
            None => {
                let graph = self.graph_name(g)?;
                Ok(Some(Derivation::asserted(triple, graph)))
            }
        }
    }

    fn asserted(&self, g: Id, triple: &[Term; 3]) -> Result<Option<Derivation>> {
        if !self.held(g, triple)? {
            return Ok(None);
        }
        self.leaf(g, triple)
    }

    fn asserted_owned(&self, g: Id, triple: [Term; 3]) -> Result<Option<Derivation>> {
        if !self.held(g, &triple)? {
            return Ok(None);
        }
        self.leaf_owned(g, triple)
    }

    /// Whether graph `g` holds `s p o`.
    fn held(&self, g: Id, t: &[Term; 3]) -> Result<bool> {
        let (Some(s), Some(p), Some(o)) = (self.id(&t[0]), self.id(&t[1]), self.id(&t[2])) else {
            return Ok(false);
        };
        self.read()?;
        Ok(self.snap.count(Perm::Gspo, &[g.0, s.0, p.0, o.0])? > 0)
    }

    /// A supported stored premise with an object of `s p ?` in graph `g`.
    fn object(&self, g: Id, s: &Term, p: &NamedNode) -> Result<Option<Derivation>> {
        let (Some(subject), Some(predicate)) = (self.id(s), self.snap.lookup_iri(p.as_str()))
        else {
            return Ok(None);
        };
        self.read()?;
        let mut found = None;
        self.scan_rows(Perm::Gspo, &[g.0, subject.0, predicate.0], |key| {
            self.read()?;
            if let Some(object) = self.decode_term(Id(key[3]))? {
                let _hold = self.workspace.hold(triple_copy_bytes(s, p, &object))?;
                let triple = [s.clone(), Term::NamedNode(p.clone()), object];
                if let Some(proof) = self.leaf_owned(g, triple)? {
                    found = Some(proof);
                    return Ok(false);
                }
            }
            Ok(true)
        })?;
        Ok(found)
    }

    /// A supported stored premise with a subject of `? p o` in graph `g`.
    fn subject(&self, g: Id, p: &NamedNode, o: &Term) -> Result<Option<Derivation>> {
        let (Some(predicate), Some(object)) = (self.snap.lookup_iri(p.as_str()), self.id(o)) else {
            return Ok(None);
        };
        self.read()?;
        let mut found = None;
        let mut rows = 0usize;
        self.scan_rows(Perm::Pos, &[predicate.0, object.0], |key| {
            let q = Perm::Pos.to_quad(key);
            if q[3] == g
                && let Some(subject) = self.decode_term(q[0])?
            {
                let _hold = self.workspace.hold(triple_copy_bytes(&subject, p, o))?;
                let triple = [subject, Term::NamedNode(p.clone()), o.clone()];
                if let Some(proof) = self.leaf_owned(g, triple)? {
                    found = Some(proof);
                    return Ok(false);
                }
            }
            rows += 1;
            if rows.is_multiple_of(1024) {
                self.read()?;
            }
            Ok(true)
        })?;
        Ok(found)
    }

    /// A derivation of `s rdf:type c` in graph `g` that RDFS on read makes: from the domain
    /// or range of a property `s` uses, or by `rdfs9` from a stored or derived type of a
    /// subclass of `c`.
    fn type_in(&self, g: Id, s: &Term, c: &NamedNode) -> Result<Option<Derivation>> {
        for range in [false, true] {
            if let Some(d) = self.by_domain_range(g, s, c, range)? {
                return Ok(Some(d));
            }
        }
        let rdf_type_bytes = rdf::TYPE
            .as_str()
            .len()
            .saturating_add(std::mem::size_of::<Term>());
        for d in self
            .schema()
            .sub_class
            .get(c)
            .into_iter()
            .flatten()
            .filter(|d| *d != c)
        {
            let premise = {
                let _typed_hold = self.workspace.hold(
                    term_bytes(s)
                        .saturating_add(rdf_type_bytes)
                        .saturating_add(named_node_bytes(d)),
                )?;
                let typed = [s.clone(), nn(rdf::TYPE), Term::NamedNode(d.clone())];
                self.asserted_owned(g, typed)?
            };
            let premise = match premise {
                Some(proof) => Some(proof),
                None => match self.by_domain_range(g, s, d, false)? {
                    Some(x) => Some(x),
                    None => self.by_domain_range(g, s, d, true)?,
                },
            };
            let Some(premise) = premise else {
                continue;
            };
            let mut premise_hold = self.workspace.hold(premise.heap_bytes())?;
            let Some(sc) = self.chain(d, rdfs::SUB_CLASS_OF, c)? else {
                continue;
            };
            premise_hold.add(
                sc.heap_bytes()
                    .saturating_add(term_bytes(s))
                    .saturating_add(rdf_type_bytes)
                    .saturating_add(named_node_bytes(c)),
            )?;
            let t = [s.clone(), nn(rdf::TYPE), Term::NamedNode(c.clone())];
            return Ok(Some(Derivation::derived(
                t,
                rule("rdfs9"),
                vec![premise, sc],
            )));
        }
        Ok(None)
    }

    /// The schema fact `a pred b` as a leaf, when the schema graph holds it or the closure
    /// of a document schema has it.
    fn schema_fact(
        &self,
        a: &NamedNode,
        pred: NamedNodeRef<'_>,
        b: &NamedNode,
    ) -> Result<Option<Derivation>> {
        Ok(match self.leaves {
            // a document's leaves are the triples of its closure, so a fact outside it is
            // no leaf
            SchemaLeaves::Document => {
                if !self.schema().closes(a, pred, b) {
                    return Ok(None);
                }
                let _fact_hold = self.workspace.hold(
                    named_node_bytes(a)
                        .saturating_add(pred.as_str().len())
                        .saturating_add(std::mem::size_of::<Term>())
                        .saturating_add(named_node_bytes(b)),
                )?;
                Some(Derivation::asserted(
                    [
                        Term::NamedNode(a.clone()),
                        nn(pred),
                        Term::NamedNode(b.clone()),
                    ],
                    None,
                ))
            }
            SchemaLeaves::Graph(g) => {
                let _fact_hold = self.workspace.hold(
                    named_node_bytes(a)
                        .saturating_add(pred.as_str().len())
                        .saturating_add(std::mem::size_of::<Term>())
                        .saturating_add(named_node_bytes(b)),
                )?;
                let t = [
                    Term::NamedNode(a.clone()),
                    nn(pred),
                    Term::NamedNode(b.clone()),
                ];
                self.asserted_owned(g, t)?
            }
            SchemaLeaves::Hidden => None,
        })
    }

    /// `a pred b` for `rdfs:subClassOf` or `rdfs:subPropertyOf` in the schema's closure: a
    /// leaf, or a chain of facts the schema graph holds joined by `rdfs11` or `rdfs5`.
    fn chain(
        &self,
        a: &NamedNode,
        pred: NamedNodeRef<'_>,
        b: &NamedNode,
    ) -> Result<Option<Derivation>> {
        let g = match self.leaves {
            SchemaLeaves::Document => return self.schema_fact(a, pred, b),
            SchemaLeaves::Graph(g) => g,
            SchemaLeaves::Hidden => return Ok(None),
        };
        // the shortest path of direct facts from a to b
        let pred_owned = pred.into_owned();
        let mut back: BTreeMap<NamedNode, NamedNode> = BTreeMap::new();
        let mut search_hold = self.workspace.hold(named_node_bytes(a))?;
        let mut queue = VecDeque::from([a.clone()]);
        let mut reached = false;
        while let Some(x) = queue.pop_front() {
            let x_bytes = named_node_bytes(&x);
            let _x_hold = search_hold.split(x_bytes);
            if &x == b {
                reached = true;
                break;
            }
            let (Some(xi), Some(pi)) = (
                self.id(&Term::NamedNode(x.clone())),
                self.snap.lookup_iri(pred.as_str()),
            ) else {
                continue;
            };
            self.read()?;
            // every neighbour counts as a read, so a wide schema meets the limit and the
            // request's controls as the rows stream
            self.scan_rows(Perm::Gspo, &[g.0, xi.0, pi.0], |k| {
                self.read()?;
                if let Some(Term::NamedNode(y)) = self.decode_term(Id(k[3]))?
                    && &y != a
                    && !back.contains_key(&y)
                {
                    let y_bytes = named_node_bytes(&y);
                    let y_hold = self.workspace.hold(y_bytes)?;
                    let supported = {
                        let _triple_hold = self.workspace.hold(
                            x_bytes
                                .saturating_add(named_node_bytes(&pred_owned))
                                .saturating_add(y_bytes),
                        )?;
                        let triple = [
                            Term::NamedNode(x.clone()),
                            Term::NamedNode(pred_owned.clone()),
                            Term::NamedNode(y.clone()),
                        ];
                        self.leaf_owned(g, triple)?.is_some()
                    };
                    if !supported {
                        return Ok(true);
                    }
                    search_hold.add(y_bytes.saturating_add(x_bytes))?;
                    search_hold.absorb(y_hold);
                    back.insert(y.clone(), x.clone());
                    queue.push_back(y);
                }
                Ok(true)
            })?;
        }
        if !reached {
            return Ok(None);
        }
        let mut path_hold = self.workspace.hold(named_node_bytes(b))?;
        let mut path = vec![b.clone()];
        while path.last() != Some(a) {
            let prev = back[path.last().unwrap()].clone();
            path_hold.add(named_node_bytes(&prev))?;
            path.push(prev);
        }
        drop(back);
        drop(queue);
        drop(search_hold);
        path.reverse();
        let name = if pred == rdfs::SUB_CLASS_OF {
            "rdfs11"
        } else {
            "rdfs5"
        };
        let leaf = |x: &NamedNode, y: &NamedNode| -> Result<Derivation> {
            let _triple_hold = self.workspace.hold(
                named_node_bytes(x)
                    .saturating_add(named_node_bytes(&pred_owned))
                    .saturating_add(named_node_bytes(y)),
            )?;
            let triple = [
                Term::NamedNode(x.clone()),
                Term::NamedNode(pred_owned.clone()),
                Term::NamedNode(y.clone()),
            ];
            self.leaf_owned(g, triple)?.ok_or_else(|| {
                crate::Error::invalid("the schema premise no longer has a supported derivation")
            })
        };
        // a sc x1, x1 sc ... sc b: fold from the right
        let n = path.len();
        let mut d_hold = self.workspace.hold(0)?;
        let mut d = leaf(&path[n - 2], &path[n - 1])?;
        d_hold.resize(d.heap_bytes())?;
        for i in (0..n - 2).rev() {
            let step = leaf(&path[i], &path[i + 1])?;
            let next_conclusion = named_node_bytes(&path[i])
                .saturating_add(named_node_bytes(&pred_owned))
                .saturating_add(named_node_bytes(&path[n - 1]));
            d_hold.add(step.heap_bytes().saturating_add(next_conclusion))?;
            d = Derivation::derived(
                [
                    Term::NamedNode(path[i].clone()),
                    Term::NamedNode(pred_owned.clone()),
                    Term::NamedNode(path[n - 1].clone()),
                ],
                rule(name),
                vec![step, d],
            );
            d_hold.resize(d.heap_bytes())?;
        }
        drop(path);
        drop(path_hold);
        drop(d_hold);
        Ok(Some(d))
    }

    /// `s rdf:type c` by `rdfs2` or `rdfs3` from a stored triple of `s` in graph `g` and
    /// a domain or range `c` of its property.
    fn by_domain_range(
        &self,
        g: Id,
        s: &Term,
        c: &NamedNode,
        range: bool,
    ) -> Result<Option<Derivation>> {
        let (m, pred, name) = if range {
            (&self.schema().range, rdfs::RANGE, "rdfs3")
        } else {
            (&self.schema().domain, rdfs::DOMAIN, "rdfs2")
        };
        for (p, classes) in m {
            if !classes.contains(c) {
                continue;
            }
            let other = if range {
                self.subject(g, p, s)?
            } else {
                self.object(g, s, p)?
            };
            let Some(data) = other else {
                continue;
            };
            let mut data_hold = self.workspace.hold(data.heap_bytes())?;
            let Some(fact) = self.schema_fact(p, pred, c)? else {
                continue;
            };
            data_hold.add(
                fact.heap_bytes()
                    .saturating_add(term_bytes(s))
                    .saturating_add(
                        rdf::TYPE
                            .as_str()
                            .len()
                            .saturating_add(std::mem::size_of::<Term>()),
                    )
                    .saturating_add(named_node_bytes(c)),
            )?;
            return Ok(Some(Derivation::derived(
                [s.clone(), nn(rdf::TYPE), Term::NamedNode(c.clone())],
                rule(name),
                vec![data, fact],
            )));
        }
        Ok(None)
    }
}

pub(super) fn budget(m: String) -> crate::error::Error {
    crate::error::Error::Component(Box::new(
        crate::error::ComponentError::new("reasoner", "reasoning-budget", m.clone())
            .with_source(ExplainBudgetExceeded(m)),
    ))
}

enum LoadedSchema<'a> {
    Borrowed(&'a RdfsSchema),
    Owned(Arc<RdfsSchema>),
}

impl std::ops::Deref for LoadedSchema<'_> {
    type Target = RdfsSchema;

    fn deref(&self) -> &RdfsSchema {
        match self {
            LoadedSchema::Borrowed(s) => s,
            LoadedSchema::Owned(s) => s,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn explain_inner<'a>(
    snap: &Snapshot,
    leaves: SchemaLeaves,
    graphs: &[Id],
    triple: &[Term; 3],
    limits: ExplainLimits,
    memory_parent: Option<sparkles_budget::ParentHandle>,
    resolve: Option<&WorkspaceResolver<'_>>,
    load_schema: impl FnOnce(
        &ExplainWorkspace,
        &mut dyn FnMut() -> Result<()>,
    ) -> Result<LoadedSchema<'a>>,
) -> Result<Explained> {
    let ExplainLimits {
        max_trees,
        max_depth,
        deadline,
        cancel,
        max_memory,
    } = limits;
    let mut cx = Cx {
        schema: None,
        snap,
        leaves,
        deadline,
        cancel,
        workspace: ExplainWorkspace::with_memory_parent(max_memory, memory_parent),
        reads: std::cell::Cell::new(0),
        resolve,
    };
    cx.check()?;
    // the trees hold at most one derivation per rule, so their decoded terms are
    // measured once they are built, before the answer keeps them
    let fits = |trees: &[Derivation]| -> Result<()> {
        if let Some(limit) = max_memory {
            let bytes: usize = trees.iter().map(Derivation::heap_bytes).sum();
            if bytes > limit {
                return Err(budget(format!(
                    "the explanation's answer needs {bytes} bytes, over its memory limit of {limit}"
                )));
            }
        }
        Ok(())
    };
    for &g in graphs {
        if let Some(proof) = cx.asserted(g, triple)? {
            let mut trees = vec![proof];
            fits(&trees)?;
            finish(&mut trees, max_depth);
            return Ok(Explained::Found { trees, more: false });
        }
    }
    if leaves == SchemaLeaves::Hidden {
        return Ok(Explained::NotFound);
    }
    let loaded = load_schema(&cx.workspace, &mut || cx.check())?;
    let schema: &RdfsSchema = &loaded;
    cx.schema = Some(schema);
    // the best tree of each top rule
    let mut tops: BTreeMap<String, Derivation> = BTreeMap::new();
    let mut tops_hold = cx.workspace.hold(0)?;
    fn keep_top<'w>(
        tops: &mut BTreeMap<String, Derivation>,
        tops_hold: &mut super::WorkspaceHold<'w>,
        d: Derivation,
        mut held: Option<super::WorkspaceHold<'w>>,
    ) -> Result<()> {
        let name = d.rule.as_ref().map(|r| r.name.clone()).unwrap_or_default();
        match tops.get(&name) {
            Some(have) if have.depth <= d.depth => {
                drop(d);
                drop(held);
                Ok(())
            }
            _ => {
                let old = tops.remove(&name);
                drop(old);
                if let Some(h) = held.take() {
                    tops_hold.absorb(h);
                }
                tops.insert(name, d);
                tops_hold.resize(
                    tops.values()
                        .map(Derivation::heap_bytes)
                        .fold(0usize, usize::saturating_add),
                )
            }
        }
    }
    let [s, p, o] = triple;
    let Term::NamedNode(p) = p else {
        return Ok(Explained::NotFound);
    };
    let pr = p.as_ref();
    // a fact of the schema's closure, which a tree cut at the depth limit asks for
    if (pr == rdfs::SUB_CLASS_OF || pr == rdfs::SUB_PROPERTY_OF)
        && let (Term::NamedNode(a), Term::NamedNode(b)) = (s, o)
        && a != b
        && let Some(d) = cx.chain(a, pr, b)?
    {
        keep_top(&mut tops, &mut tops_hold, d, None)?;
    }
    if pr == rdf::TYPE
        && let Term::NamedNode(c) = o
    {
        let rdf_type_bytes = rdf::TYPE
            .as_str()
            .len()
            .saturating_add(std::mem::size_of::<Term>());
        let subs = schema.sub_class.get(c);
        for &g in graphs {
            for range in [false, true] {
                if let Some(d) = cx.by_domain_range(g, s, c, range)? {
                    keep_top(&mut tops, &mut tops_hold, d, None)?;
                }
            }
            for d in subs.into_iter().flatten().filter(|d| *d != c) {
                let premise = {
                    let _typed_hold = cx.workspace.hold(
                        term_bytes(s)
                            .saturating_add(rdf_type_bytes)
                            .saturating_add(named_node_bytes(d)),
                    )?;
                    let typed = [s.clone(), nn(rdf::TYPE), Term::NamedNode(d.clone())];
                    cx.asserted_owned(g, typed)?
                };
                let premise = match premise {
                    Some(proof) => Some(proof),
                    None => match cx.by_domain_range(g, s, d, false)? {
                        Some(x) => Some(x),
                        None => cx.by_domain_range(g, s, d, true)?,
                    },
                };
                let Some(premise) = premise else {
                    continue;
                };
                let mut premise_hold = cx.workspace.hold(premise.heap_bytes())?;
                let Some(sc) = cx.chain(d, rdfs::SUB_CLASS_OF, c)? else {
                    continue;
                };
                premise_hold.add(sc.heap_bytes().saturating_add(triple_bytes(triple)))?;
                let derived = Derivation::derived(triple.clone(), rule("rdfs9"), vec![premise, sc]);
                keep_top(&mut tops, &mut tops_hold, derived, Some(premise_hold))?;
            }
        }
    }
    if pr != rdfs::SUB_CLASS_OF && pr != rdfs::SUB_PROPERTY_OF {
        let subs = schema.sub_prop.get(p);
        'graphs: for &g in graphs {
            for q in subs.into_iter().flatten().filter(|q| *q != p) {
                let asserted = {
                    let _stored_hold = cx.workspace.hold(triple_copy_bytes(s, q, o))?;
                    let stored = [s.clone(), Term::NamedNode(q.clone()), o.clone()];
                    cx.asserted_owned(g, stored)?
                };
                let premise = if let Some(proof) = asserted {
                    proof
                } else if q.as_ref() == rdf::TYPE
                    && let Term::NamedNode(c) = o
                    && let Some(d) = cx.type_in(g, s, c)?
                {
                    // RDFS on read answers a superproperty of rdf:type from a type
                    // it derives, as well as from a stored one
                    d
                } else {
                    continue;
                };
                let mut premise_hold = cx.workspace.hold(premise.heap_bytes())?;
                let Some(sp) = cx.chain(q, rdfs::SUB_PROPERTY_OF, p)? else {
                    continue;
                };
                premise_hold.add(sp.heap_bytes().saturating_add(triple_bytes(triple)))?;
                let derived = Derivation::derived(triple.clone(), rule("rdfs7"), vec![premise, sp]);
                keep_top(&mut tops, &mut tops_hold, derived, Some(premise_hold))?;
                break 'graphs;
            }
        }
    }
    drop(tops_hold);
    if tops.is_empty() {
        return Ok(Explained::NotFound);
    }
    let mut all: Vec<Derivation> = tops.into_values().collect();
    fits(&all)?;
    all.sort_by_key(|d| d.depth);
    let more = all.len() > max_trees.max(1);
    all.truncate(max_trees.max(1));
    finish(&mut all, max_depth);
    cx.check()?;
    Ok(Explained::Found { trees: all, more })
}

impl RdfsOnRead {
    /// Explain `triple` over `graphs`, checking visible asserted answers before building a
    /// cold graph-backed schema and sharing one [`ExplainWorkspace`] across proof search
    /// and `resolve`.
    pub fn explain_resolving(
        &self,
        snap: &Snapshot,
        leaves: SchemaLeaves,
        graphs: &[Id],
        triple: &[Term; 3],
        limits: ExplainLimits,
        resolve: Option<&WorkspaceResolver<'_>>,
    ) -> Result<Explained> {
        self.explain_resolving_with_parent(snap, leaves, graphs, triple, limits, resolve, None)
    }

    /// Explain an RDFS answer under an enclosing pool allowance.
    #[allow(clippy::too_many_arguments)]
    pub fn explain_resolving_with_parent(
        &self,
        snap: &Snapshot,
        leaves: SchemaLeaves,
        graphs: &[Id],
        triple: &[Term; 3],
        limits: ExplainLimits,
        resolve: Option<&WorkspaceResolver<'_>>,
        memory_parent: Option<sparkles_budget::ParentHandle>,
    ) -> Result<Explained> {
        explain_inner(
            snap,
            leaves,
            graphs,
            triple,
            limits,
            memory_parent,
            resolve,
            |workspace, check| {
                self.schema_controlled_with_workspace(snap, workspace, check)
                    .map(LoadedSchema::Owned)
            },
        )
    }
}

impl RdfsSchema {
    /// Explain why RDFS on read with this schema answers `triple` in one of `graphs`, the
    /// graphs the caller reads, from the snapshot of the caller's view: up to `max_trees`
    /// derivations whose top rules differ, cut at the depth of `limits`. A triple a graph holds is
    /// its own leaf.
    pub fn explain(
        &self,
        snap: &Snapshot,
        leaves: SchemaLeaves,
        graphs: &[Id],
        triple: &[Term; 3],
        limits: ExplainLimits,
    ) -> Result<Explained> {
        self.explain_resolving(snap, leaves, graphs, triple, limits, None)
    }

    /// Explain with supported leaves selected before the best proof of a rule is kept.
    pub fn explain_resolving(
        &self,
        snap: &Snapshot,
        leaves: SchemaLeaves,
        graphs: &[Id],
        triple: &[Term; 3],
        limits: ExplainLimits,
        resolve: Option<&AssertedResolver<'_>>,
    ) -> Result<Explained> {
        let wrapped =
            resolve.map(|r| move |g: Id, t: &[Term; 3], _workspace: &ExplainWorkspace| r(g, t));
        let resolve = wrapped.as_ref().map(|r| r as &WorkspaceResolver<'_>);
        explain_inner(
            snap,
            leaves,
            graphs,
            triple,
            limits,
            None,
            resolve,
            |_, _| Ok(LoadedSchema::Borrowed(self)),
        )
    }
}
