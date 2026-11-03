//! The class taxonomy: the classes grouped into equivalence classes and the direct
//! subsumptions between the groups, computed from the saturated contexts by the
//! transitive reduction of ELK (JAR 2014, §6).

use super::Saturation;
use super::index::{Ex, Node as Ix, TOP};
use crate::model::*;
use crate::vocab;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

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
/// The classes between two polls of the run's deadline and cancellation.
const POLL_EVERY: usize = 4096;
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

/// Why building the taxonomy stopped.
#[derive(Debug)]
pub(super) enum Halt {
    /// the deadline or a cancellation
    Stopped,
    /// the budget refused the bytes the taxonomy needs
    Memory(usize),
}

/// Builds the taxonomy of the classes of the classifier's signature from its saturated
/// contexts, on `threads` threads. It polls `stop` as it goes, and asks `budget` for the
/// estimated bytes of the taxonomy and of what building it holds once it has read the
/// subsumers, which are most of them.
///
/// The classes are numbered in the order of their IRIs, and each class's subsumers, its
/// equivalence class and, for the first class of each, the direct superclasses are
/// computed from the saturated contexts alone, so the answer is the same for every
/// number of threads.
pub(super) fn build(
    k: &Saturation,
    consistent: bool,
    threads: usize,
    stop: &(dyn Fn() -> bool + Sync),
    budget: &dyn Fn(usize) -> bool,
) -> Result<(Taxonomy, usize), Halt> {
    let mut classes: Vec<(&Class, Ex)> = k
        .interner
        .met_classes()
        .map(|e| match k.interner.nodes[e as usize] {
            Ix::Class(id) => (&k.interner.classes[id as usize], e),
            _ => unreachable!("a class's expression"),
        })
        .collect();
    classes.sort_unstable_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    let mut top = vec![Class::thing()];
    let mut bottom = vec![Class::nothing()];
    if !consistent {
        // every class in the bottom node and the lookup
        let bytes = classes.len()
            * (2 * std::mem::size_of::<Class>() + std::mem::size_of::<(Class, usize)>() * 8 / 7)
            + classes
                .iter()
                .map(|(c, _)| 2 * c.as_str().len())
                .sum::<usize>();
        if !budget(bytes) {
            return Err(Halt::Memory(bytes));
        }
        bottom.push(Class::thing());
        bottom.extend(classes.into_iter().map(|(c, _)| c.clone()));
        sort_classes(&mut bottom);
        let mut of = FxHashMap::default();
        for c in &bottom[2..] {
            of.insert(c.clone(), BOTTOM_NODE);
        }
        return Ok((
            Taxonomy {
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
            },
            bytes,
        ));
    }
    let n = classes.len();
    // the position of each class's expression in the IRI order
    let mut pos: Vec<u32> = vec![NONE; k.interner.len()];
    for (i, &(_, e)) in classes.iter().enumerate() {
        pos[e as usize] = i as u32;
    }
    let stopped = AtomicBool::new(false);
    // `i` counts the items of a chunk, so each chunk polls when it starts
    let halt = |i: usize| {
        if i.is_multiple_of(POLL_EVERY) && (stopped.load(Ordering::Relaxed) || stop()) {
            stopped.store(true, Ordering::Relaxed);
            return true;
        }
        false
    };
    // the named subsumers of each class as positions, sorted, owl:Thing left out, and
    // whether the class is unsatisfiable
    let mut subs: Vec<(bool, Vec<u32>)> = vec![(false, Vec::new()); n];
    let mut work: Vec<(usize, &mut (bool, Vec<u32>))> = subs.iter_mut().enumerate().collect();
    super::par_chunks(&mut work, threads, |chunk| {
        for (j, (i, slot)) in chunk.iter_mut().enumerate() {
            if halt(j) {
                return;
            }
            let e = classes[*i].1;
            let (b, s) = k.named_subsumers(e).unwrap_or((false, vec![e]));
            let mut v: Vec<u32> = s
                .into_iter()
                .map(|x| pos[x as usize])
                .filter(|&x| x != NONE)
                .collect();
            v.sort_unstable();
            **slot = (b, v);
        }
    });
    drop(work);
    if stopped.load(Ordering::Relaxed) {
        return Err(Halt::Stopped);
    }
    let bytes = bytes_of(&classes, &subs, pos.len());
    if !budget(bytes) {
        return Err(Halt::Memory(bytes));
    }
    // the classes equivalent to owl:Thing
    let mut at_top = vec![false; n];
    if let Some((_, s)) = k.named_subsumers(TOP) {
        for x in s {
            if let Some(&p) = pos.get(x as usize)
                && p != NONE
            {
                at_top[p as usize] = true;
            }
        }
    }
    let has = |a: u32, b: u32| a == b || subs[a as usize].1.binary_search(&b).is_ok();
    // each satisfiable class's equivalence class, by its first class in IRI order;
    // unsatisfiable classes and those equivalent to owl:Thing get NONE
    let mut key: Vec<u32> = vec![NONE; n];
    let mut work: Vec<(usize, &mut u32)> = key.iter_mut().enumerate().collect();
    super::par_chunks(&mut work, threads, |chunk| {
        for (j, (i, slot)) in chunk.iter_mut().enumerate() {
            if halt(j) {
                return;
            }
            let i32 = *i as u32;
            let (unsat, s) = &subs[*i];
            if *unsat || at_top[*i] {
                continue;
            }
            // the subsumers are sorted, so the first one equivalent to the class is its
            // equivalence class's first class
            **slot = s
                .iter()
                .copied()
                .find(|&b| has(b, i32))
                .unwrap_or(i32)
                .min(i32);
        }
    });
    drop(work);
    if stopped.load(Ordering::Relaxed) {
        return Err(Halt::Stopped);
    }
    // the nodes, in the order of their first classes
    let mut node_of: Vec<u32> = vec![NONE; n];
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
    let mut reps: Vec<u32> = Vec::new();
    for i in 0..n {
        let (c, _) = classes[i];
        if subs[i].0 {
            bottom.push(c.clone());
        } else if at_top[i] {
            top.push(c.clone());
        } else if key[i] == i as u32 {
            node_of[i] = nodes.len() as u32;
            reps.push(i as u32);
            nodes.push(Node {
                classes: vec![c.clone()],
                parents: Vec::new(),
                children: Vec::new(),
            });
        } else {
            let at = node_of[key[i] as usize];
            node_of[i] = at;
            nodes[at as usize].classes.push(c.clone());
        }
    }
    // the direct superclasses of each node: the minimal nodes among its subsumers
    let mut parents: Vec<Vec<usize>> = vec![Vec::new(); reps.len()];
    let mut work: Vec<(u32, &mut Vec<usize>)> =
        reps.iter().copied().zip(parents.iter_mut()).collect();
    super::par_chunks(&mut work, threads, |chunk| {
        for (j, (r, slot)) in chunk.iter_mut().enumerate() {
            if halt(j) {
                return;
            }
            **slot = direct_parents(*r, &subs, &key, &node_of, &has);
        }
    });
    drop(work);
    if stopped.load(Ordering::Relaxed) {
        return Err(Halt::Stopped);
    }
    // the links, each held as a parent and as a child, join the estimate before the
    // children are built
    let links: usize = parents.iter().map(|ps| ps.len()).sum();
    let bytes = bytes + 2 * links * std::mem::size_of::<usize>();
    if !budget(bytes) {
        return Err(Halt::Memory(bytes));
    }
    let mut fan_in = vec![0usize; nodes.len()];
    for ps in &parents {
        for &p in ps {
            fan_in[p] += 1;
        }
    }
    for (node, k) in nodes.iter_mut().zip(fan_in) {
        node.children.reserve_exact(k);
    }
    for (j, ps) in parents.into_iter().enumerate() {
        for &p in &ps {
            nodes[p].children.push(j + 2);
        }
        nodes[j + 2].parents = ps;
    }
    sort_classes(&mut top);
    sort_classes(&mut bottom);
    nodes[TOP_NODE].classes = top;
    nodes[BOTTOM_NODE].classes = bottom;
    let mut of = FxHashMap::with_capacity_and_hasher(n, Default::default());
    for (i, node) in nodes.iter().enumerate() {
        for c in &node.classes {
            of.insert(c.clone(), i);
        }
    }
    Ok((Taxonomy { nodes, of }, bytes))
}

/// The estimated bytes of a taxonomy and of what building it holds before its links: the
/// subsumers read, the tables by class and by expression, and for each class its two
/// copies in the nodes and the lookup. The build adds the links once it has them.
fn bytes_of(classes: &[(&Class, Ex)], subs: &[(bool, Vec<u32>)], exprs: usize) -> usize {
    let per_class = std::mem::size_of::<(&Class, Ex)>()
        + std::mem::size_of::<(bool, Vec<u32>)>()
        + 3 * 4
        + 1
        + std::mem::size_of::<Node>()
        + 2 * std::mem::size_of::<Class>()
        + std::mem::size_of::<(Class, usize)>() * 8 / 7;
    let iris: usize = classes.iter().map(|(c, _)| 2 * c.as_str().len()).sum();
    let read: usize = subs.iter().map(|(_, v)| v.capacity() * 4).sum();
    classes.len() * per_class + iris + read + exprs * 4
}

/// No position or node.
const NONE: u32 = u32::MAX;

/// The direct superclasses of the node whose first class is `r`, as node indices,
/// sorted: the minimal nodes among the subsumers of `r`.
fn direct_parents(
    r: u32,
    subs: &[(bool, Vec<u32>)],
    key: &[u32],
    node_of: &[u32],
    has: &dyn Fn(u32, u32) -> bool,
) -> Vec<usize> {
    let mut cands: Vec<u32> = subs[r as usize]
        .1
        .iter()
        .map(|&b| key[b as usize])
        .filter(|&g| g != NONE && g != r)
        .collect();
    cands.sort_unstable();
    cands.dedup();
    // more subsumers first, so that lower nodes tend to come first
    cands.sort_by_key(|&g| std::cmp::Reverse(subs[g as usize].1.len()));
    let mut direct: Vec<u32> = Vec::new();
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
        // many direct parents: mark the nodes above some candidate instead, which costs
        // the candidates' subsumers rather than candidates times parents
        let mut covered: FxHashSet<u32> = FxHashSet::default();
        for &d in &cands {
            for &b in &subs[d as usize].1 {
                let kb = key[b as usize];
                if kb != NONE && kb != d {
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
    let mut parents: Vec<usize> = if direct.is_empty() {
        vec![TOP_NODE]
    } else {
        direct
            .iter()
            .map(|&g| node_of[g as usize] as usize)
            .collect()
    };
    parents.sort_unstable();
    parents
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::Limits;
    use std::sync::atomic::AtomicUsize;

    fn c(s: &str) -> Class {
        Class::new(format!("http://ex.org/{s}"))
    }

    fn saturated(axioms: &[Axiom]) -> Saturation {
        let kinds: Vec<&AxiomKind> = axioms.iter().map(|a| &a.kind).collect();
        let mut k = Saturation::new();
        k.classify(&kinds, None, &Limits::default()).unwrap();
        k
    }

    #[test]
    fn the_estimate_counts_every_link_of_a_wide_hierarchy() {
        // every lower class is below every upper class through an existential that no
        // name stands for, so each has all the upper classes as direct parents
        let n = 400;
        let via = ClassExpression::some(ObjectProperty::new("http://ex.org/r"), c("Y"));
        let mut axioms = Vec::new();
        for i in 0..n {
            axioms.push(Axiom::sub_class_of(c(&format!("L{i}")), via.clone()));
            axioms.push(Axiom::sub_class_of(via.clone(), c(&format!("U{i}"))));
        }
        let k = saturated(&axioms);
        let (t, bytes) = build(&k, true, 1, &|| false, &|_| true).unwrap();
        let links: usize = t
            .nodes()
            .iter()
            .map(|node| node.parents.capacity() + node.children.capacity())
            .sum();
        assert!(links >= 2 * n * n);
        assert!(
            bytes >= links * std::mem::size_of::<usize>(),
            "{bytes} for {links} links"
        );
        // a budget that does not cover the links stops the build
        let below = bytes - 1;
        assert!(matches!(
            build(&k, true, 1, &|| false, &|b| b <= below),
            Err(Halt::Memory(_))
        ));
    }

    #[test]
    fn an_inconsistent_taxonomy_asks_the_budget() {
        let mut axioms = vec![Axiom::sub_class_of(Class::thing(), Class::nothing())];
        for i in 0..1000 {
            axioms.push(Axiom::sub_class_of(c(&format!("A{i}")), c("B")));
        }
        let k = saturated(&axioms);
        let (t, bytes) = build(&k, false, 1, &|| false, &|_| true).unwrap();
        assert!(t.bottom().classes.len() > 1000);
        assert!(bytes >= 1000 * 2 * std::mem::size_of::<Class>(), "{bytes}");
        assert!(matches!(
            build(&k, false, 1, &|| false, &|b| b < bytes),
            Err(Halt::Memory(_))
        ));
    }

    #[test]
    fn every_stage_polls_whatever_the_positions_of_its_nodes() {
        // the first class is unsatisfiable and the others come in equivalent pairs, so
        // every node's first class has an odd position
        let mut axioms = vec![Axiom::sub_class_of(c("A00"), Class::nothing())];
        for i in (1..21).step_by(2) {
            axioms.push(Axiom::equivalent_classes([
                c(&format!("A{i:02}")).into(),
                c(&format!("A{:02}", i + 1)).into(),
            ]));
        }
        let k = saturated(&axioms);
        let polls = AtomicUsize::new(0);
        let (t, _) = build(
            &k,
            true,
            1,
            &|| {
                polls.fetch_add(1, Ordering::Relaxed);
                false
            },
            &|_| true,
        )
        .unwrap();
        assert_eq!(t.equivalents(&c("A01")), &[c("A01"), c("A02")]);
        let polls = polls.into_inner();
        assert!(polls >= 3, "{polls} polls");
        // a stop in the last stage is heard
        let calls = AtomicUsize::new(0);
        let stop = || calls.fetch_add(1, Ordering::Relaxed) + 1 >= 3;
        assert!(matches!(
            build(&k, true, 1, &stop, &|_| true),
            Err(Halt::Stopped)
        ));
    }
}
