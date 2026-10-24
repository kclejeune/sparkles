//! Explanations of what RDFS on read answers (spec U10 §3.3).
//!
//! RDFS on read derives a triple in one step from a stored triple of the same graph and
//! the schema, so a tree has one data premise and the schema facts it used. The rules are
//! those of the W3C RDF 1.1 Semantics that Jena's `MatchRDFS` applies: `rdfs9` for a type
//! from a stored type and the closed class hierarchy, `rdfs2` and `rdfs3` for a type from
//! the domain or range its declaring property has, and `rdfs7` for a triple from a stored
//! triple of a subproperty. A subclass or subproperty fact the schema graph does not hold
//! directly is derived by `rdfs11` or `rdfs5` from a chain of facts it holds.

use super::RdfsSchema;
use crate::error::Result;
use crate::explain::{
    Derivation, ExplainBudgetExceeded, ExplainLimits, Explained, RuleRef, finish,
};
use crate::id::Id;
use crate::index::Perm;
use crate::store::Snapshot;
use oxrdf::vocab::{rdf, rdfs};
use oxrdf::{NamedNode, NamedNodeRef, Term};
use std::collections::{BTreeMap, VecDeque};
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

struct Cx<'a> {
    schema: &'a RdfsSchema,
    snap: &'a Snapshot,
    leaves: SchemaLeaves,
    deadline: Option<Instant>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    reads: std::cell::Cell<usize>,
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

    fn graph_name(&self, g: Id) -> Option<String> {
        if g == Id::DEFAULT_GRAPH {
            return Some("default".to_string());
        }
        match self.snap.term(g)? {
            Term::NamedNode(n) => Some(n.into_string()),
            _ => None,
        }
    }

    /// Whether graph `g` holds `s p o`.
    fn held(&self, g: Id, t: &[Term; 3]) -> Result<bool> {
        let (Some(s), Some(p), Some(o)) = (self.id(&t[0]), self.id(&t[1]), self.id(&t[2])) else {
            return Ok(false);
        };
        self.read()?;
        Ok(self.snap.count(Perm::Gspo, &[g.0, s.0, p.0, o.0])? > 0)
    }

    /// An object of `s p ?` in graph `g`.
    fn object(&self, g: Id, s: &Term, p: &NamedNode) -> Result<Option<Term>> {
        let (Some(s), Some(p)) = (self.id(s), self.snap.lookup_iri(p.as_str())) else {
            return Ok(None);
        };
        self.read()?;
        let mut found = None;
        self.snap.scan(Perm::Gspo, &[g.0, s.0, p.0], |c| {
            let o = match c {
                crate::store::Chunk::Block(b, st, _) => b.cols[3][st],
                crate::store::Chunk::Row(k) => k[3],
            };
            found = Some(Id(o));
            Ok(false)
        })?;
        Ok(found.and_then(|o| self.snap.term(o)))
    }

    /// A subject of `? p o` in graph `g`.
    fn subject(&self, g: Id, p: &NamedNode, o: &Term) -> Result<Option<Term>> {
        let (Some(p), Some(o)) = (self.snap.lookup_iri(p.as_str()), self.id(o)) else {
            return Ok(None);
        };
        self.read()?;
        // the rows of `? p o` stream in subject order, with the graph last, and the scan
        // stops at the first row of `g`; every 1,024 rows count as one more read
        let mut found = None;
        let mut rows = 0usize;
        self.scan_rows(Perm::Pos, &[p.0, o.0], |k| {
            let q = Perm::Pos.to_quad(k);
            if q[3] == g {
                found = Some(q[0]);
                return Ok(false);
            }
            rows += 1;
            if rows.is_multiple_of(1024) {
                self.read()?;
            }
            Ok(true)
        })?;
        Ok(found.and_then(|s| self.snap.term(s)))
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
        let subs: Vec<NamedNode> = RdfsSchema::get(&self.schema.sub_class, c.as_ref())
            .into_iter()
            .flatten()
            .filter(|d| *d != c)
            .cloned()
            .collect();
        for d in &subs {
            let typed = [s.clone(), nn(rdf::TYPE), Term::NamedNode(d.clone())];
            let premise = if self.held(g, &typed)? {
                Some(Derivation::asserted(typed, self.graph_name(g)))
            } else {
                match self.by_domain_range(g, s, d, false)? {
                    Some(x) => Some(x),
                    None => self.by_domain_range(g, s, d, true)?,
                }
            };
            let Some(premise) = premise else {
                continue;
            };
            let Some(sc) = self.chain(d, rdfs::SUB_CLASS_OF, c)? else {
                continue;
            };
            let t = [s.clone(), nn(rdf::TYPE), Term::NamedNode(c.clone())];
            return Ok(Some(Derivation::derived(
                t,
                rule("rdfs9"),
                vec![premise, sc],
            )));
        }
        Ok(None)
    }

    /// The schema fact `a pred b` that the schema holds directly, as a leaf.
    fn schema_fact(
        &self,
        a: &NamedNode,
        pred: NamedNodeRef<'_>,
        b: &NamedNode,
    ) -> Result<Option<Derivation>> {
        let t = [
            Term::NamedNode(a.clone()),
            nn(pred),
            Term::NamedNode(b.clone()),
        ];
        Ok(match self.leaves {
            SchemaLeaves::Document => Some(Derivation::asserted(t, None)),
            SchemaLeaves::Graph(g) => {
                if self.held(g, &t)? {
                    Some(Derivation::asserted(t, self.graph_name(g)))
                } else {
                    None
                }
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
        let mut queue = VecDeque::from([a.clone()]);
        let mut reached = false;
        while let Some(x) = queue.pop_front() {
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
                if let Some(Term::NamedNode(y)) = self.snap.term(Id(k[3]))
                    && &y != a
                    && !back.contains_key(&y)
                {
                    back.insert(y.clone(), x.clone());
                    queue.push_back(y);
                }
                Ok(true)
            })?;
        }
        if !reached {
            return Ok(None);
        }
        let mut path = vec![b.clone()];
        while path.last() != Some(a) {
            let prev = back[path.last().unwrap()].clone();
            path.push(prev);
        }
        path.reverse();
        let name = if pred == rdfs::SUB_CLASS_OF {
            "rdfs11"
        } else {
            "rdfs5"
        };
        let leaf = |x: &NamedNode, y: &NamedNode| {
            Derivation::asserted(
                [
                    Term::NamedNode(x.clone()),
                    Term::NamedNode(pred_owned.clone()),
                    Term::NamedNode(y.clone()),
                ],
                self.graph_name(g),
            )
        };
        // a sc x1, x1 sc ... sc b: fold from the right
        let n = path.len();
        let mut d = leaf(&path[n - 2], &path[n - 1]);
        for i in (0..n - 2).rev() {
            d = Derivation::derived(
                [
                    Term::NamedNode(path[i].clone()),
                    Term::NamedNode(pred_owned.clone()),
                    Term::NamedNode(path[n - 1].clone()),
                ],
                rule(name),
                vec![leaf(&path[i], &path[i + 1]), d],
            );
        }
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
            (&self.schema.range, rdfs::RANGE, "rdfs3")
        } else {
            (&self.schema.domain, rdfs::DOMAIN, "rdfs2")
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
            let Some(x) = other else {
                continue;
            };
            let Some(fact) = self.schema_fact(p, pred, c)? else {
                continue;
            };
            let data = if range {
                [x, Term::NamedNode(p.clone()), s.clone()]
            } else {
                [s.clone(), Term::NamedNode(p.clone()), x]
            };
            return Ok(Some(Derivation::derived(
                [s.clone(), nn(rdf::TYPE), Term::NamedNode(c.clone())],
                rule(name),
                vec![Derivation::asserted(data, self.graph_name(g)), fact],
            )));
        }
        Ok(None)
    }
}

fn budget(m: String) -> crate::error::Error {
    crate::error::Error::Component(Box::new(
        crate::error::ComponentError::new("reasoner", "reasoning-budget", m.clone())
            .with_source(ExplainBudgetExceeded(m)),
    ))
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
        let ExplainLimits {
            max_trees,
            max_depth,
            deadline,
            cancel,
            max_memory,
        } = limits;
        let cx = Cx {
            schema: self,
            snap,
            leaves,
            deadline,
            cancel,
            reads: std::cell::Cell::new(0),
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
            if cx.held(g, triple)? {
                let mut trees = vec![Derivation::asserted(triple.clone(), cx.graph_name(g))];
                fits(&trees)?;
                finish(&mut trees, max_depth);
                return Ok(Explained::Found { trees, more: false });
            }
        }
        // the best tree of each top rule
        let mut tops: BTreeMap<String, Derivation> = BTreeMap::new();
        let mut keep = |d: Derivation| {
            let name = d.rule.as_ref().map(|r| r.name.clone()).unwrap_or_default();
            match tops.get(&name) {
                Some(have) if have.depth <= d.depth => {}
                _ => {
                    tops.insert(name, d);
                }
            }
        };
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
            keep(d);
        }
        if pr == rdf::TYPE
            && let Term::NamedNode(c) = o
        {
            let subs: Vec<NamedNode> = Self::get(&self.sub_class, c.as_ref())
                .into_iter()
                .flatten()
                .filter(|d| *d != c)
                .cloned()
                .collect();
            for &g in graphs {
                for range in [false, true] {
                    if let Some(d) = cx.by_domain_range(g, s, c, range)? {
                        keep(d);
                    }
                }
                for d in &subs {
                    let typed = [s.clone(), nn(rdf::TYPE), Term::NamedNode(d.clone())];
                    let premise = if cx.held(g, &typed)? {
                        Some(Derivation::asserted(typed, cx.graph_name(g)))
                    } else {
                        match cx.by_domain_range(g, s, d, false)? {
                            Some(x) => Some(x),
                            None => cx.by_domain_range(g, s, d, true)?,
                        }
                    };
                    let Some(premise) = premise else {
                        continue;
                    };
                    let Some(sc) = cx.chain(d, rdfs::SUB_CLASS_OF, c)? else {
                        continue;
                    };
                    keep(Derivation::derived(
                        triple.clone(),
                        rule("rdfs9"),
                        vec![premise, sc],
                    ));
                }
            }
        }
        if pr != rdfs::SUB_CLASS_OF && pr != rdfs::SUB_PROPERTY_OF {
            let subs: Vec<NamedNode> = Self::get(&self.sub_prop, pr)
                .into_iter()
                .flatten()
                .filter(|q| *q != p)
                .cloned()
                .collect();
            'graphs: for &g in graphs {
                for q in &subs {
                    let stored = [s.clone(), Term::NamedNode(q.clone()), o.clone()];
                    let premise = if cx.held(g, &stored)? {
                        Derivation::asserted(stored, cx.graph_name(g))
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
                    let Some(sp) = cx.chain(q, rdfs::SUB_PROPERTY_OF, p)? else {
                        continue;
                    };
                    keep(Derivation::derived(
                        triple.clone(),
                        rule("rdfs7"),
                        vec![premise, sp],
                    ));
                    break 'graphs;
                }
            }
        }
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
}
