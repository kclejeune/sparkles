//! The class taxonomy: the classes grouped into equivalence classes and the direct
//! subsumptions between the groups, computed from the saturated contexts by the
//! transitive reduction of ELK (JAR 2014, §6).

use super::Saturation;
use super::index::{Ex, Node as Ix, TOP};
use crate::model::*;
use crate::vocab;
use rustc_hash::FxHashSet;
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
    /// the node and the place in it of each class of the signature, in the order of
    /// their IRIs, which [`Taxonomy::node_of`] searches by the names the nodes hold
    at: Vec<(u32, u32)>,
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
            iri => self
                .at
                .binary_search_by(|&(n, k)| {
                    self.nodes[n as usize].classes[k as usize].as_str().cmp(iri)
                })
                .ok()
                .map(|i| self.at[i].0 as usize),
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
///
/// `close` is set for a partial answer, whose contexts did not all saturate: the rows of
/// subsumers are closed first, as [`close_rows`] says, since the rest of the build needs
/// the closed rows that a whole saturation gives.
pub(super) fn build(
    k: &Saturation,
    consistent: bool,
    threads: usize,
    close: bool,
    stop: &(dyn Fn() -> bool + Sync),
    budget: &dyn Fn(usize) -> bool,
) -> Result<(Taxonomy, usize), Halt> {
    // the classes the normalization met, by class id in parallel chunks
    let met = &k.interner.met;
    let found = super::par_map(met, threads, super::MIN_CLASSES, |at, chunk| {
        let mut out = Vec::new();
        for (j, &m) in chunk.iter().enumerate() {
            let id = (at + j) as u32;
            if m && let Some(e) = k.interner.get(&Ix::Class(id)) {
                out.push((&k.interner.classes[id as usize], e));
            }
        }
        out
    });
    let classes: Vec<(&Class, Ex)> = sort_by_iri(joined(found), threads);
    if !consistent {
        // every class in the bottom node and the lookup
        let bytes = classes.len()
            * (std::mem::size_of::<Class>() + std::mem::size_of::<(u32, u32)>())
            + classes.iter().map(|(c, _)| c.as_str().len()).sum::<usize>();
        if !budget(bytes) {
            return Err(Halt::Memory(bytes));
        }
        // owl:Nothing and owl:Thing first, then the classes, which are in IRI order
        let mut bottom = vec![Class::nothing(), Class::thing()];
        bottom.extend(
            super::par_map(&classes, threads, super::MIN_CLASSES, |_, chunk| {
                chunk.iter().map(|(c, _)| (*c).clone()).collect::<Vec<_>>()
            })
            .into_iter()
            .flatten(),
        );
        let at = (0..classes.len() as u32)
            .map(|i| (BOTTOM_NODE as u32, i + 2))
            .collect();
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
                at,
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
    super::par_chunks(&mut work, threads, super::MIN_CLASSES, |chunk| {
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
    // the named subsumers of owl:Thing, as positions
    let mut top: Vec<u32> = k
        .named_subsumers(TOP)
        .map(|(_, s)| {
            s.into_iter()
                .filter_map(|x| pos.get(x as usize).copied().filter(|&p| p != NONE))
                .collect()
        })
        .unwrap_or_default();
    if close {
        top = close_rows(&mut subs, &top, budget)?;
    }
    let bytes = bytes_of(&classes, &subs, pos.len());
    if !budget(bytes) {
        return Err(Halt::Memory(bytes));
    }
    // the classes equivalent to owl:Thing
    let mut at_top = vec![false; n];
    for p in top {
        at_top[p as usize] = true;
    }
    let has = |a: u32, b: u32| a == b || subs[a as usize].1.binary_search(&b).is_ok();
    // each satisfiable class's equivalence class, by its first class in IRI order;
    // unsatisfiable classes and those equivalent to owl:Thing get NONE
    let mut key: Vec<u32> = vec![NONE; n];
    let mut work: Vec<(usize, &mut u32)> = key.iter_mut().enumerate().collect();
    super::par_chunks(&mut work, threads, super::MIN_CLASSES, |chunk| {
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
    // the nodes, in the order of their first classes: number them and place each class
    // in its node, after owl:Thing in the top node and owl:Nothing in the bottom node, on
    // one pass of integers, then copy the names into the nodes in parallel chunks
    let mut node_of: Vec<u32> = vec![NONE; n];
    let mut at: Vec<(u32, u32)> = vec![(0, 0); n];
    let mut size: Vec<u32> = vec![1, 1];
    let mut reps: Vec<u32> = Vec::new();
    for i in 0..n {
        let node = if subs[i].0 {
            BOTTOM_NODE as u32
        } else if at_top[i] {
            TOP_NODE as u32
        } else if key[i] == i as u32 {
            reps.push(i as u32);
            size.push(0);
            node_of[i] = (size.len() - 1) as u32;
            node_of[i]
        } else {
            node_of[i] = node_of[key[i] as usize];
            node_of[i]
        };
        at[i] = (node, size[node as usize]);
        size[node as usize] += 1;
    }
    // the classes of each node, as positions, in its order
    let mut start: Vec<usize> = Vec::with_capacity(size.len() + 1);
    let mut sum = 0usize;
    for &z in &size {
        start.push(sum);
        sum += z as usize;
    }
    start.push(sum);
    let mut members: Vec<u32> = vec![NONE; sum];
    members[start[TOP_NODE]] = THING;
    members[start[BOTTOM_NODE]] = NOTHING;
    for (i, &(node, slot)) in at.iter().enumerate() {
        members[start[node as usize] + slot as usize] = i as u32;
    }
    let ids: Vec<usize> = (0..size.len()).collect();
    let nodes = super::par_map(&ids, threads, super::MIN_CLASSES, |_, chunk| {
        chunk
            .iter()
            .map(|&j| Node {
                classes: members[start[j]..start[j + 1]]
                    .iter()
                    .map(|&i| match i {
                        THING => Class::thing(),
                        NOTHING => Class::nothing(),
                        i => classes[i as usize].0.clone(),
                    })
                    .collect(),
                parents: Vec::new(),
                children: Vec::new(),
            })
            .collect::<Vec<_>>()
    });
    let mut nodes: Vec<Node> = joined(nodes);
    drop(members);
    // the direct superclasses of each node: the minimal nodes among its subsumers
    let mut parents: Vec<Vec<usize>> = vec![Vec::new(); reps.len()];
    let mut work: Vec<(u32, &mut Vec<usize>)> =
        reps.iter().copied().zip(parents.iter_mut()).collect();
    super::par_chunks(&mut work, threads, super::MIN_CLASSES, |chunk| {
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
    Ok((Taxonomy { nodes, at }, bytes))
}

/// The classes sorted by IRI on up to `threads` threads: a sample sort, whose buckets
/// are ranges of IRIs sorted in parallel and joined in order. The IRIs are distinct, so
/// the order is the one a sequential sort gives.
fn sort_by_iri(mut v: Vec<(&Class, Ex)>, threads: usize) -> Vec<(&Class, Ex)> {
    let cmp = |a: &(&Class, Ex), b: &(&Class, Ex)| a.0.as_str().cmp(b.0.as_str());
    let buckets = threads.clamp(1, v.len().div_ceil(SORT_MIN).max(1));
    if buckets == 1 {
        v.sort_unstable_by(cmp);
        return v;
    }
    // splitters from an evenly spaced sample
    let step = (v.len() / (buckets * 32)).max(1);
    let mut sample: Vec<&str> = v.iter().step_by(step).map(|(c, _)| c.as_str()).collect();
    sample.sort_unstable();
    let splitters: Vec<&str> = (1..buckets)
        .map(|b| sample[b * sample.len() / buckets])
        .collect();
    let bucket = |c: &Class| splitters.partition_point(|s| *s <= c.as_str());
    // each chunk's classes by bucket, then each bucket joined from the chunks and sorted
    let parts: Vec<Vec<Vec<(&Class, Ex)>>> =
        super::par_map(&v, threads, super::MIN_CLASSES, |_, chunk| {
            let mut out: Vec<Vec<(&Class, Ex)>> = vec![Vec::new(); buckets];
            for &x in chunk {
                out[bucket(x.0)].push(x);
            }
            out
        });
    drop(v);
    let sorted: Vec<Vec<(&Class, Ex)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..buckets)
            .map(|b| {
                let parts = &parts;
                s.spawn(move || {
                    let mut all: Vec<(&Class, Ex)> =
                        parts.iter().flat_map(|p| p[b].iter().copied()).collect();
                    all.sort_unstable_by(cmp);
                    all
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
            .collect()
    });
    joined(sorted)
}

/// The vectors joined in order, by moving their items.
fn joined<T>(parts: Vec<Vec<T>>) -> Vec<T> {
    let mut out = Vec::with_capacity(parts.iter().map(Vec::len).sum());
    for p in parts {
        out.extend(p);
    }
    out
}

/// The fewest classes a bucket of the parallel sort takes; fewer in tests, so that small
/// ontologies take the parallel path.
const SORT_MIN: usize = if cfg!(test) { 256 } else { 16 * 1024 };

/// The places of `owl:Thing` and `owl:Nothing` among a node's classes.
const THING: u32 = u32::MAX - 1;
const NOTHING: u32 = u32::MAX - 2;

/// The estimated bytes of a taxonomy and of what building it holds before its links: the
/// subsumers read, the tables by class and by expression, the tables that place the
/// classes in their nodes, and for each class its node, its copy in the node and its
/// entry in the lookup. The build adds the links once it has them.
fn bytes_of(classes: &[(&Class, Ex)], subs: &[(bool, Vec<u32>)], exprs: usize) -> usize {
    let per_class = std::mem::size_of::<(&Class, Ex)>()
        + std::mem::size_of::<(bool, Vec<u32>)>()
        + 3 * 4
        + 1
        + 2 * 4
        + std::mem::size_of::<usize>()
        + std::mem::size_of::<Node>()
        + std::mem::size_of::<Class>()
        + std::mem::size_of::<(u32, u32)>();
    let iris: usize = classes.iter().map(|(c, _)| c.as_str().len()).sum();
    let read: usize = subs.iter().map(|(_, v)| v.capacity() * 4).sum();
    classes.len() * per_class + iris + read + exprs * 4
}

/// No position or node.
const NONE: u32 = u32::MAX;

/// Writes the sorted union of sorted `a` and `b`, each without repeats, to `out`.
fn merge_into(a: &[u32], b: &[u32], out: &mut Vec<u32>) {
    out.clear();
    out.reserve(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
}

/// Closes the rows of a partial answer, whose subsumers are each entailed but whose
/// contexts did not all saturate. Each row takes the rows of the classes in it, through
/// the components of the graph of rows in the order that Tarjan's algorithm closes them,
/// so that the classes a row reaches are in it and each class of a cycle has the same
/// row. A row that reaches an unsatisfiable class is unsatisfiable. The closed row of
/// owl:Thing, `top`, joins every row, and the answer is that closed row. The rows grow
/// only by entailed subsumers, and a row that was saturated is closed already. The bytes
/// of the closed rows are asked of `budget` as they grow.
fn close_rows(
    subs: &mut [(bool, Vec<u32>)],
    top: &[u32],
    budget: &dyn Fn(usize) -> bool,
) -> Result<Vec<u32>, Halt> {
    let n = subs.len();
    // Tarjan's algorithm without recursion: the components, each after those it reaches
    let mut index = vec![NONE; n];
    let mut low = vec![0u32; n];
    let mut on = vec![false; n];
    let mut comp = vec![NONE; n];
    let mut comps: Vec<Vec<u32>> = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    let mut next = 0u32;
    for root in 0..n {
        if index[root] != NONE {
            continue;
        }
        // (node, the next edge to follow)
        let mut calls: Vec<(u32, usize)> = vec![(root as u32, 0)];
        index[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root as u32);
        on[root] = true;
        while let Some(&mut (v, ref mut e)) = calls.last_mut() {
            let vu = v as usize;
            if let Some(&w) = subs[vu].1.get(*e) {
                *e += 1;
                let wu = w as usize;
                if index[wu] == NONE {
                    index[wu] = next;
                    low[wu] = next;
                    next += 1;
                    stack.push(w);
                    on[wu] = true;
                    calls.push((w, 0));
                } else if on[wu] {
                    low[vu] = low[vu].min(index[wu]);
                }
                continue;
            }
            calls.pop();
            if let Some(&(p, _)) = calls.last() {
                low[p as usize] = low[p as usize].min(low[vu]);
            }
            if low[vu] == index[vu] {
                let id = comps.len() as u32;
                let mut members = Vec::new();
                while let Some(w) = stack.pop() {
                    on[w as usize] = false;
                    comp[w as usize] = id;
                    members.push(w);
                    if w == v {
                        break;
                    }
                }
                comps.push(members);
            }
        }
    }
    // each component's closed row, with whether it is unsatisfiable, in the order made,
    // which has every component it reaches made before it. A row grows by sorted merges
    // with the rows of the distinct components it reaches, and the bytes held, the
    // scratch row included, are asked of `budget` before each merge that passes the last
    // grant by a mebibyte.
    let mut rows: Vec<(bool, Vec<u32>)> = Vec::with_capacity(comps.len());
    let mut held = 0usize;
    let mut asked = 0usize;
    let ask = |bytes: usize, asked: &mut usize| -> Result<(), Halt> {
        if bytes > *asked + (1 << 20) {
            if !budget(bytes) {
                return Err(Halt::Memory(bytes));
            }
            *asked = bytes;
        }
        Ok(())
    };
    let mut seen = vec![NONE; comps.len()];
    let mut scratch: Vec<u32> = Vec::new();
    for members in &comps {
        let id = rows.len() as u32;
        let mut unsat = false;
        let mut row: Vec<u32> = members.clone();
        row.sort_unstable();
        for &m in members {
            unsat |= subs[m as usize].0;
            for &w in &subs[m as usize].1 {
                let c = comp[w as usize];
                if c == id || seen[c as usize] == id {
                    continue;
                }
                seen[c as usize] = id;
                let (u, other) = &rows[c as usize];
                unsat |= *u;
                ask(held + (row.len() + other.len()) * 4 * 2, &mut asked)?;
                merge_into(&row, other, &mut scratch);
                std::mem::swap(&mut row, &mut scratch);
            }
        }
        row.shrink_to_fit();
        held += row.len() * 4 + 32;
        ask(held, &mut asked)?;
        rows.push((unsat, row));
    }
    drop(scratch);
    // the closed row of owl:Thing joins every row
    let mut t: Vec<u32> = top.to_vec();
    t.sort_unstable();
    t.dedup();
    let mut t_unsat = false;
    let mut scratch: Vec<u32> = Vec::new();
    for &p in top {
        let (u, r) = &rows[comp[p as usize] as usize];
        t_unsat |= *u;
        ask(held + (t.len() + r.len()) * 4 * 2, &mut asked)?;
        merge_into(&t, r, &mut scratch);
        std::mem::swap(&mut t, &mut scratch);
    }
    drop(scratch);
    t.shrink_to_fit();
    // the rows replace the read ones, which are freed as they go
    for (i, slot) in subs.iter_mut().enumerate() {
        let (u, r) = &rows[comp[i] as usize];
        let mut row = Vec::new();
        merge_into(r, &t, &mut row);
        row.shrink_to_fit();
        held += row.len() * 4;
        ask(held, &mut asked)?;
        *slot = (*u || t_unsat, row);
    }
    Ok(t)
}

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
        let (t, bytes) = build(&k, true, 1, false, &|| false, &|_| true).unwrap();
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
            build(&k, true, 1, false, &|| false, &|b| b <= below),
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
        let (t, bytes) = build(&k, false, 1, false, &|| false, &|_| true).unwrap();
        assert!(t.bottom().classes.len() > 1000);
        assert!(bytes >= 1000 * std::mem::size_of::<Class>(), "{bytes}");
        assert!(matches!(
            build(&k, false, 1, false, &|| false, &|b| b < bytes),
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
            false,
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
            build(&k, true, 1, false, &stop, &|_| true),
            Err(Halt::Stopped)
        ));
    }

    #[test]
    fn the_parallel_sort_gives_the_sequential_order() {
        // IRIs that share long prefixes and differ in their middle or end
        let classes: Vec<Class> = (0..20_000u64)
            .map(|i| {
                let x = i.wrapping_mul(0x9e37_79b9_7f4a_7c15) % 1_000_003;
                c(&format!("{}/{x:07}/{}", x % 7, i % 13))
            })
            .collect();
        let v: Vec<(&Class, Ex)> = classes.iter().zip(0..).collect();
        let mut want = v.clone();
        want.sort_unstable_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        for threads in [1, 2, 3, 8, 64] {
            assert_eq!(sort_by_iri(v.clone(), threads), want, "{threads} threads");
        }
    }

    #[test]
    fn a_large_taxonomy_is_the_same_on_every_number_of_threads() {
        // enough classes for the parallel sort and the parallel nodes: a forest of
        // chains with equivalent pairs, classes equivalent to owl:Thing and
        // unsatisfiable classes
        let n = 6_000;
        let mut axioms = Vec::new();
        for i in 0..n {
            let a = c(&format!("C{:06}", (i * 7919) % n));
            if i % 50 == 0 {
                axioms.push(Axiom::sub_class_of(Class::thing(), a));
            } else if i % 97 == 0 {
                axioms.push(Axiom::sub_class_of(a, Class::nothing()));
            } else if i % 3 == 0 {
                axioms.push(Axiom::equivalent_classes([
                    a.into(),
                    c(&format!("C{:06}", (i * 7919 + 1) % n)).into(),
                ]));
            } else {
                axioms.push(Axiom::sub_class_of(
                    a,
                    c(&format!("C{:06}", (i * 7919 + 13) % n)),
                ));
            }
        }
        let k = saturated(&axioms);
        let (one, _) = build(&k, true, 1, false, &|| false, &|_| true).unwrap();
        for threads in [2, 8] {
            let (t, _) = build(&k, true, threads, false, &|| false, &|_| true).unwrap();
            assert!(t == one, "{threads} threads");
        }
        // every class's lookup finds the node that holds it
        let mut seen = 0;
        for (i, node) in one.nodes().iter().enumerate() {
            for class in &node.classes {
                let want = match class.as_str() {
                    vocab::OWL_THING => TOP_NODE,
                    vocab::OWL_NOTHING => BOTTOM_NODE,
                    _ => i,
                };
                assert_eq!(one.node_of(class), Some(want), "{class:?}");
                seen += 1;
            }
            assert!(
                node.classes
                    .windows(2)
                    .all(|w| w[0].as_str() < w[1].as_str()
                        || matches!(w[0].as_str(), vocab::OWL_THING | vocab::OWL_NOTHING))
            );
        }
        assert!(seen >= n);
        assert!(one.bottom().classes.len() > 1 && one.top().classes.len() > 1);
        assert_eq!(one.node_of(&c("absent")), None);
        // an inconsistent ontology puts every class in the bottom node on any number of
        // threads
        let (bad, _) = build(&k, false, 8, false, &|| false, &|_| true).unwrap();
        let (bad1, _) = build(&k, false, 1, false, &|| false, &|_| true).unwrap();
        assert!(bad == bad1);
        assert_eq!(bad.node_of(&c("C000001")), Some(BOTTOM_NODE));
        assert_eq!(bad.node_of(&Class::thing()), Some(BOTTOM_NODE));
    }

    #[test]
    fn closing_dense_rows_asks_the_budget() {
        // rows that are closed already, each class below every later one
        let n = 512u32;
        let rows =
            || -> Vec<(bool, Vec<u32>)> { (0..n).map(|i| (false, (i..n).collect())).collect() };
        let out_bytes = (n as usize * (n as usize + 1) / 2) * 4;
        let most = std::cell::Cell::new(0usize);
        let mut subs = rows();
        let top = close_rows(&mut subs, &[], &|b| {
            most.set(most.get().max(b));
            true
        })
        .unwrap();
        assert!(top.is_empty());
        for (i, (u, r)) in subs.iter().enumerate() {
            assert!(!u);
            assert_eq!(r.len(), n as usize - i);
        }
        assert!(
            most.get() >= out_bytes / 2,
            "asked for {} bytes",
            most.get()
        );
        assert!(
            most.get() <= 4 * out_bytes,
            "asked for {} bytes",
            most.get()
        );
        // a budget below the closed rows stops the closure
        let mut subs = rows();
        assert!(matches!(
            close_rows(&mut subs, &[], &|b| b <= out_bytes / 4),
            Err(Halt::Memory(_))
        ));
    }

    #[test]
    fn closing_sparse_rows_asks_the_budget() {
        // rows of one class each, which no merge joins
        let n = 350_000u32;
        let mut subs: Vec<(bool, Vec<u32>)> = (0..n).map(|i| (false, vec![i])).collect();
        let first = std::cell::Cell::new(0usize);
        let r = close_rows(&mut subs, &[], &|b| {
            if first.get() == 0 {
                first.set(b);
            }
            false
        });
        assert!(matches!(r, Err(Halt::Memory(_))));
        assert!(
            first.get() <= 2 << 20,
            "first asked for {} bytes",
            first.get()
        );
    }
}
