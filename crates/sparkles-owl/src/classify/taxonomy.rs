//! The class taxonomy: the classes grouped into equivalence classes and the direct
//! subsumptions between the groups, computed from the saturated contexts by the
//! transitive reduction of ELK (JAR 2014, §6).

use super::Classifier;
use super::index::{Ex, Node as Ix, TOP};
use crate::model::*;
use crate::vocab;
use rustc_hash::FxHashMap;
use std::collections::BTreeSet;

/// A node of the taxonomy: equivalent classes, sorted by IRI, and the direct
/// superclasses and subclasses as node indices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub classes: Vec<Class>,
    pub parents: Vec<usize>,
    pub children: Vec<usize>,
}

/// The inferred class hierarchy. Node 0 holds `owl:Thing` and the classes equivalent to
/// it, node 1 holds `owl:Nothing` and the unsatisfiable classes. The other nodes are
/// sorted by their first class. The bottom node has no parents or children: every
/// class is its superclass. In an inconsistent ontology every class, `owl:Thing`
/// included, is in the bottom node and node 0 is empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Taxonomy {
    nodes: Vec<Node>,
    of: FxHashMap<Class, usize>,
}

pub const TOP_NODE: usize = 0;
/// The direct parents the greedy reduction finds before it marks instead.
const GREEDY_MAX: usize = 32;
pub const BOTTOM_NODE: usize = 1;

impl Taxonomy {
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    pub fn node(&self, i: usize) -> &Node {
        &self.nodes[i]
    }

    pub fn top(&self) -> &Node {
        &self.nodes[TOP_NODE]
    }

    pub fn bottom(&self) -> &Node {
        &self.nodes[BOTTOM_NODE]
    }

    /// The node of a class of the signature, `owl:Thing` or `owl:Nothing`.
    pub fn node_of(&self, c: &Class) -> Option<usize> {
        match c.as_str() {
            vocab::OWL_THING if self.nodes[TOP_NODE].classes.is_empty() => Some(BOTTOM_NODE),
            vocab::OWL_THING => Some(TOP_NODE),
            vocab::OWL_NOTHING => Some(BOTTOM_NODE),
            _ => self.of.get(c).copied(),
        }
    }

    /// The classes of the signature, `owl:Thing` and `owl:Nothing` left out.
    pub fn classes(&self) -> impl Iterator<Item = &Class> {
        self.nodes
            .iter()
            .flat_map(|n| n.classes.iter())
            .filter(|c| !matches!(c.as_str(), vocab::OWL_THING | vocab::OWL_NOTHING))
    }

    /// The unsatisfiable classes, sorted.
    pub fn unsatisfiable(&self) -> impl Iterator<Item = &Class> {
        self.bottom()
            .classes
            .iter()
            .filter(|c| !matches!(c.as_str(), vocab::OWL_NOTHING | vocab::OWL_THING))
    }

    pub fn is_unsatisfiable(&self, c: &Class) -> bool {
        self.node_of(c) == Some(BOTTOM_NODE)
    }

    /// The classes equivalent to a class, itself included.
    pub fn equivalents(&self, c: &Class) -> &[Class] {
        self.node_of(c).map_or(&[], |n| &self.nodes[n].classes)
    }

    /// The nodes of a class's direct superclasses.
    pub fn parents(&self, c: &Class) -> &[usize] {
        self.node_of(c).map_or(&[], |n| &self.nodes[n].parents)
    }

    /// The nodes of a class's direct subclasses.
    pub fn children(&self, c: &Class) -> &[usize] {
        self.node_of(c).map_or(&[], |n| &self.nodes[n].children)
    }

    /// The nodes above a node, the node itself left out, sorted.
    pub fn ancestors(&self, n: usize) -> Vec<usize> {
        self.walk(n, |x| &x.parents)
    }

    /// The nodes below a node, the node itself and the bottom node left out, sorted.
    pub fn descendants(&self, n: usize) -> Vec<usize> {
        self.walk(n, |x| &x.children)
    }

    fn walk(&self, n: usize, next: impl Fn(&Node) -> &Vec<usize>) -> Vec<usize> {
        let mut seen = BTreeSet::new();
        let mut stack: Vec<usize> = next(&self.nodes[n]).clone();
        while let Some(x) = stack.pop() {
            if seen.insert(x) {
                stack.extend(next(&self.nodes[x]).iter().copied());
            }
        }
        seen.into_iter().collect()
    }

    /// Whether `sub` is subsumed by `sup`.
    pub fn is_subclass(&self, sub: &Class, sup: &Class) -> bool {
        let (Some(a), Some(b)) = (self.node_of(sub), self.node_of(sup)) else {
            return false;
        };
        a == b || a == BOTTOM_NODE || b == TOP_NODE || self.ancestors(a).contains(&b)
    }

    /// The taxonomy as axioms: an equivalence for each node of several classes, with
    /// `owl:Thing` and `owl:Nothing` in theirs, and a subclass axiom from the first class
    /// of each node to the first class of each of its parents. These are the axioms
    /// that ELK and ROBOT write for a taxonomy.
    pub fn axioms(&self) -> Vec<Axiom> {
        let mut out = Vec::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.classes.len() > 1 {
                out.push(Axiom::equivalent_classes(
                    n.classes.iter().map(|c| ClassExpression::Class(c.clone())),
                ));
            }
            if i == BOTTOM_NODE {
                continue;
            }
            let Some(rep) = n.classes.first() else {
                continue;
            };
            for &p in &n.parents {
                if let Some(q) = self.nodes[p].classes.first() {
                    out.push(Axiom::sub_class_of(rep.clone(), q.clone()));
                }
            }
        }
        out
    }
}

/// The node of `owl:Thing` or `owl:Nothing` first, then the classes by IRI.
fn sort_classes(cs: &mut [Class]) {
    cs.sort_by(|a, b| {
        let builtin = |c: &Class| !matches!(c.as_str(), vocab::OWL_THING | vocab::OWL_NOTHING);
        (builtin(a), a.as_str()).cmp(&(builtin(b), b.as_str()))
    });
}

/// Builds the taxonomy of the classes of the classifier's signature from its saturated
/// contexts.
pub(super) fn build(k: &Classifier, consistent: bool) -> Taxonomy {
    let mut classes: Vec<(Class, Ex)> = k
        .classes
        .keys()
        .filter_map(|c| k.interner.class_ex(c).map(|e| (c.clone(), e)))
        .collect();
    classes.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    let mut top = vec![Class::thing()];
    let mut bottom = vec![Class::nothing()];
    if !consistent {
        bottom.push(Class::thing());
        bottom.extend(classes.into_iter().map(|(c, _)| c));
        sort_classes(&mut bottom);
        let mut of = FxHashMap::default();
        for c in &bottom[2..] {
            of.insert(c.clone(), BOTTOM_NODE);
        }
        return Taxonomy {
            nodes: vec![
                Node {
                    classes: Vec::new(),
                    parents: Vec::new(),
                    children: Vec::new(),
                },
                Node {
                    classes: bottom,
                    parents: Vec::new(),
                    children: Vec::new(),
                },
            ],
            of,
        };
    }
    // the named subsumers of each class, sorted for lookups
    let mut subs: FxHashMap<Ex, Vec<Ex>> = FxHashMap::default();
    let mut unsat: FxHashMap<Ex, bool> = FxHashMap::default();
    for &(_, e) in &classes {
        let (b, mut s) = k.named_subsumers(e).unwrap_or((false, vec![e, TOP]));
        s.sort_unstable();
        unsat.insert(e, b);
        subs.insert(e, s);
    }
    let top_subs: Vec<Ex> = {
        let (_, mut s) = k.named_subsumers(TOP).unwrap_or((false, vec![TOP]));
        s.sort_unstable();
        s
    };
    let has = |a: Ex, b: Ex| -> bool {
        b == TOP || a == b || subs.get(&a).is_some_and(|s| s.binary_search(&b).is_ok())
    };
    // group the satisfiable classes into equivalence classes, each keyed by its
    // smallest expression
    let mut key: FxHashMap<Ex, Ex> = FxHashMap::default();
    let mut groups: FxHashMap<Ex, Vec<Class>> = FxHashMap::default();
    let name_of: FxHashMap<Ex, &Class> = classes.iter().map(|(c, e)| (*e, c)).collect();
    for (c, e) in &classes {
        if unsat[e] {
            bottom.push(c.clone());
            continue;
        }
        if top_subs.binary_search(e).is_ok() {
            top.push(c.clone());
            key.insert(*e, TOP);
            continue;
        }
        let eq = subs[e]
            .iter()
            .copied()
            .filter(|&b| b != TOP && matches!(k.interner.nodes[b as usize], Ix::Class(_)))
            .filter(|&b| has(b, *e))
            .min()
            .unwrap_or(*e)
            .min(*e);
        key.insert(*e, eq);
        groups.entry(eq).or_default().push(c.clone());
    }
    let mut reps: Vec<Ex> = groups.keys().copied().collect();
    reps.sort_by(|a, b| {
        let fa = groups[a].iter().map(|c| c.as_str()).min();
        let fb = groups[b].iter().map(|c| c.as_str()).min();
        fa.cmp(&fb)
    });
    let mut index: FxHashMap<Ex, usize> = FxHashMap::default();
    index.insert(TOP, TOP_NODE);
    let mut nodes = vec![
        Node {
            classes: Vec::new(),
            parents: Vec::new(),
            children: Vec::new(),
        },
        Node {
            classes: Vec::new(),
            parents: Vec::new(),
            children: Vec::new(),
        },
    ];
    for &r in &reps {
        index.insert(r, nodes.len());
        let mut cs = groups.remove(&r).unwrap_or_default();
        sort_classes(&mut cs);
        nodes.push(Node {
            classes: cs,
            parents: Vec::new(),
            children: Vec::new(),
        });
    }
    // the direct superclasses of each group: the minimal groups among its subsumers
    for &r in &reps {
        let mut cands: Vec<Ex> = subs[&r]
            .iter()
            .filter(|&&b| b != TOP && name_of.contains_key(&b))
            .filter_map(|b| key.get(b).copied())
            .filter(|&g| g != r && g != TOP)
            .collect();
        cands.sort_unstable();
        cands.dedup();
        // more subsumers first, so that lower groups tend to come first
        cands.sort_by_key(|g| std::cmp::Reverse(subs.get(g).map_or(0, |s| s.len())));
        let mut direct: Vec<Ex> = Vec::new();
        for &g in &cands {
            if direct.iter().any(|&d| has(d, g)) {
                continue;
            }
            direct.retain(|&d| !has(g, d));
            direct.push(g);
            if direct.len() > GREEDY_MAX {
                break;
            }
        }
        if direct.len() > GREEDY_MAX {
            // many direct parents: mark the groups above some candidate instead, which
            // costs the candidates' subsumers rather than candidates times parents
            let mut covered: rustc_hash::FxHashSet<Ex> = rustc_hash::FxHashSet::default();
            for &d in &cands {
                for b in &subs[&d] {
                    if let Some(&kb) = key.get(b)
                        && kb != d
                        && kb != TOP
                    {
                        covered.insert(kb);
                    }
                }
            }
            direct = cands
                .iter()
                .copied()
                .filter(|g| !covered.contains(g))
                .collect();
        }
        let me = index[&r];
        let mut parents: Vec<usize> = if direct.is_empty() {
            vec![TOP_NODE]
        } else {
            direct.iter().map(|g| index[g]).collect()
        };
        parents.sort_unstable();
        nodes[me].parents = parents;
    }
    for i in 0..nodes.len() {
        for p in nodes[i].parents.clone() {
            nodes[p].children.push(i);
        }
    }
    for n in &mut nodes {
        n.children.sort_unstable();
    }
    sort_classes(&mut top);
    sort_classes(&mut bottom);
    nodes[TOP_NODE].classes = top;
    nodes[BOTTOM_NODE].classes = bottom;
    let mut of = FxHashMap::default();
    for (i, n) in nodes.iter().enumerate() {
        for c in &n.classes {
            of.insert(c.clone(), i);
        }
    }
    Taxonomy { nodes, of }
}
