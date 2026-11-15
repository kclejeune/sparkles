//! The strata of a program (item 839's plan, §1.3).
//!
//! The dependency graph is the one demand's `negation_on_a_cycle` reads, extended to
//! declared relations and to the IR's negation. Its nodes are the declared relations, each
//! constant predicate, `rdf:type` split by constant class, and four nodes for what reads or
//! gives every predicate and every class. Every head node of a rule has an edge to every
//! read of the rule: its atoms, its negated atoms and `noValue` patterns (negative), the
//! `rdf:first` and `rdf:rest` facts of the lists its builtins and head actions read, and the
//! facts each `listForAll` tests. List structure is a positive read, as demand counts it,
//! and the negative list builtins that read lists a rule may give are reported in
//! [`Strata::open_lists`] instead.
//!
//! A rule is evaluated in parts ([`Part`]), each taking the stratum of its heads. A rule
//! with an existential or a `makeTemp` call is one part, since each of its instances makes
//! new nodes that its heads share, and its head nodes get edges both ways, so they share a
//! stratum. Any other rule is one part per head, since its body gives each head the same
//! values in any stratum.

use super::{Head, IAtom, Literal, Pred, RelId, Rule, Vocab};
use crate::budget::{Budget, BudgetFailure, Held, table_bytes, table_capacity};
use crate::builtins::{BuiltinKind, HeadAction};
use crate::demand::{Read, list_reads};
use crate::engine::Slot;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::fmt;
use std::sync::Arc;

const READS_ALL: u32 = 0;
const GIVES_ALL: u32 = 1;
const READS_TYPES: u32 = 2;
const GIVES_TYPES: u32 = 3;
const SPECIAL: u32 = 4;
/// The bytes per node of the strata's scratch: the offsets and the strata of the
/// components, Tarjan's index, low link, stack flag, component, stack and walk, and a
/// component's members and header.
pub(super) const SCRATCH_PER_NODE: usize = 128;

/// A negation that depends on its own rule's head: the rule, the index of the negated
/// literal in its body, and the rules whose heads are on the cycle's component, in
/// program order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NegativeCycle {
    pub rule: usize,
    pub literal: usize,
    pub rules: Vec<usize>,
}

/// A part of a rule: the heads that are evaluated together, and their stratum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Part {
    pub rule: usize,
    pub heads: SmallVec<[usize; 2]>,
    pub stratum: u32,
}

/// The strata of a program, charged to its budget while they live.
pub(crate) struct Strata {
    /// the parts of every rule, in rule order and then by first head
    pub parts: Vec<Part>,
    /// the number of strata, at least 1
    pub count: u32,
    /// whether the program stratifies, or the first negation found on a cycle
    pub stratified: Result<(), NegativeCycle>,
    /// whether a rule has a negated atom or a `noValue`
    pub has_negation: bool,
    /// the rules whose `listNotContains`, `listLength` or `listForAll` reads list
    /// structure that a head of the program may give
    pub open_lists: Vec<usize>,
    _held: Held,
}

impl fmt::Debug for Strata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Strata")
            .field("parts", &self.parts)
            .field("count", &self.count)
            .field("stratified", &self.stratified)
            .field("has_negation", &self.has_negation)
            .field("open_lists", &self.open_lists)
            .finish()
    }
}

impl Strata {
    /// The stratum of each rule's parts, as `(rule, heads, stratum)`.
    pub fn of(
        rules: &[Rule],
        vocab: &Vocab,
        budget: &Arc<Budget>,
    ) -> Result<Strata, BudgetFailure> {
        Graph::build(rules, vocab, budget)?.strata(rules, vocab)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Key {
    /// a constant predicate, with its constant class for `rdf:type`
    Triple(u64, Option<u64>),
    Rel(RelId),
}

/// A read of a rule.
#[derive(Clone, Copy)]
struct Dep {
    node: u32,
    negative: bool,
    /// the literal of the rule, for a negative read
    literal: u32,
}

/// A part of a rule in the graph: the rule, its head nodes and its heads.
type GraphPart = (usize, SmallVec<[u32; 2]>, SmallVec<[usize; 2]>);

struct Graph {
    ids: FxHashMap<Key, u32>,
    /// the kind of each node from [`SPECIAL`] on
    keys: Vec<Key>,
    /// `(from, to, negative)`, then sorted by `from`
    edges: Vec<(u32, u32, bool)>,
    /// the negative edges with the rule and literal that give them
    negative: Vec<(u32, u32, u32, u32)>,
    /// each part's rule, head nodes and heads
    parts: Vec<GraphPart>,
    has_negation: bool,
    /// a head gives `rdf:first`, `rdf:rest` or every predicate
    gives_lists: bool,
    _held: Held,
}

/// Bounds of the graph of `rules`, from their literals: nodes, edges, parts.
fn bounds(rules: &[Rule]) -> (usize, usize, usize, usize) {
    let (mut nodes, mut edges, mut parts, mut negative) = (SPECIAL as usize, 0usize, 0, 0);
    for r in rules {
        // a literal reads one node, and a builtin up to two (a `noValue` pattern, the
        // facts of `listForAll`), and a rule that reads lists `rdf:first` and `rdf:rest`
        let deps = r.body.len() * 2 + 2;
        let heads = r.head.len();
        nodes += deps + heads;
        edges += heads * deps + 2 * heads;
        negative += heads * r.body.len();
        parts += heads.max(1);
    }
    // the bridges: every node to `GIVES_ALL`, `READS_ALL` to every node, and a class to
    // `GIVES_TYPES` and from `READS_TYPES`
    edges += 4 * nodes + 3;
    (nodes, edges, parts, negative)
}

impl Graph {
    fn build(rules: &[Rule], vocab: &Vocab, budget: &Arc<Budget>) -> Result<Graph, BudgetFailure> {
        let (nodes, edges, parts, negative) = bounds(rules);
        let map = table_bytes(table_capacity(nodes), size_of::<(Key, u32)>());
        let bytes = map
            + nodes * size_of::<Key>()
            + edges * size_of::<(u32, u32, bool)>()
            + negative * size_of::<(u32, u32, u32, u32)>()
            + parts * size_of::<GraphPart>()
            // the scratch of the strata: the offsets, the strata of the components and
            // Tarjan's arrays, stack, walk and components
            + nodes * SCRATCH_PER_NODE;
        let held = Held::try_new_exact(budget, bytes)?;
        let mut g = Graph {
            ids: FxHashMap::with_capacity_and_hasher(nodes, Default::default()),
            keys: Vec::with_capacity(nodes),
            edges: Vec::with_capacity(edges),
            negative: Vec::with_capacity(negative),
            parts: Vec::with_capacity(parts),
            has_negation: false,
            gives_lists: false,
            _held: held,
        };
        let mut deps: Vec<Dep> = Vec::new();
        let mut heads: Vec<u32> = Vec::new();
        for (ri, r) in rules.iter().enumerate() {
            deps.clear();
            heads.clear();
            let mut reads_lists = false;
            for (li, l) in r.body.iter().enumerate() {
                let li = li as u32;
                match l {
                    Literal::Pos(a) => deps.push(Dep {
                        node: g.atom(a, false, vocab),
                        negative: false,
                        literal: li,
                    }),
                    Literal::Neg(a) => {
                        g.has_negation = true;
                        deps.push(Dep {
                            node: g.atom(a, false, vocab),
                            negative: true,
                            literal: li,
                        });
                    }
                    Literal::Builtin(b) => {
                        if b.kind == BuiltinKind::NoValue {
                            g.has_negation = true;
                            let p = b.args.get(1).copied().unwrap_or(Slot::Any);
                            let o = b.args.get(2).copied().unwrap_or(Slot::Any);
                            deps.push(Dep {
                                node: g.triple(p, o, false, vocab),
                                negative: true,
                                literal: li,
                            });
                        }
                        for read in list_reads(b.kind, &b.args, None) {
                            match read {
                                Read::List => reads_lists = true,
                                Read::Facts(_, p) => deps.push(Dep {
                                    node: g.triple(p, Slot::Any, false, vocab),
                                    negative: false,
                                    literal: li,
                                }),
                            }
                        }
                    }
                }
            }
            for h in &r.head {
                let node = match h {
                    Head::Atom(a) => g.atom(a, true, vocab),
                    Head::Action(action, args) => {
                        reads_lists = true;
                        match action {
                            HeadAction::ListMapAsSubject => g.triple(args[1], args[2], true, vocab),
                            HeadAction::ListMapAsObject => {
                                g.triple(args[1], Slot::Any, true, vocab)
                            }
                        }
                    }
                };
                if node == GIVES_ALL
                    || matches!(g.key(node), Some(Key::Triple(p, _)) if p == vocab.rdf_first || p == vocab.rdf_rest)
                {
                    g.gives_lists = true;
                }
                heads.push(node);
            }
            if reads_lists {
                for p in [vocab.rdf_first, vocab.rdf_rest] {
                    deps.push(Dep {
                        node: g.triple(Slot::Const(p), Slot::Any, false, vocab),
                        negative: false,
                        literal: u32::MAX,
                    });
                }
            }
            for &h in &heads {
                for d in &deps {
                    g.edges.push((h, d.node, d.negative));
                    if d.negative {
                        g.negative.push((h, d.node, ri as u32, d.literal));
                    }
                }
            }
            for group in groups(r) {
                let nodes: SmallVec<[u32; 2]> = group.iter().map(|&h| heads[h]).collect();
                for w in nodes.windows(2) {
                    g.edges.push((w[0], w[1], false));
                    g.edges.push((w[1], w[0], false));
                }
                g.parts.push((ri, nodes, group));
            }
        }
        g.bridges();
        debug_assert!(g.edges.len() <= edges && g.keys.len() + SPECIAL as usize <= nodes);
        Ok(g)
    }

    fn key(&self, n: u32) -> Option<Key> {
        n.checked_sub(SPECIAL).map(|i| self.keys[i as usize])
    }

    fn node(&mut self, key: Key) -> u32 {
        let n = self.keys.len() as u32 + SPECIAL;
        let id = *self.ids.entry(key).or_insert(n);
        if id == n {
            self.keys.push(key);
        }
        id
    }

    fn atom(&mut self, a: &IAtom, head: bool, vocab: &Vocab) -> u32 {
        match a.pred {
            Pred::Triple => self.triple(a.args[1], a.args[2], head, vocab),
            Pred::Rel(r) => self.node(Key::Rel(r)),
        }
    }

    /// The node of a triple pattern with predicate `p` and object `o`.
    fn triple(&mut self, p: Slot, o: Slot, head: bool, vocab: &Vocab) -> u32 {
        let (all, types) = if head {
            (GIVES_ALL, GIVES_TYPES)
        } else {
            (READS_ALL, READS_TYPES)
        };
        match p {
            Slot::Const(t) if Some(t) == vocab.rdf_type => match o {
                Slot::Const(c) => self.node(Key::Triple(t, Some(c))),
                _ => types,
            },
            Slot::Const(p) => self.node(Key::Triple(p, None)),
            _ => all,
        }
    }

    /// Demand's edges between the special nodes and the constant ones: a constant
    /// predicate reads what a head with a variable predicate gives, an atom with a variable
    /// predicate reads every constant one, and classes likewise for `rdf:type` with a
    /// variable class. Relations take no part, since no triple pattern reaches them.
    fn bridges(&mut self) {
        let n = self.keys.len() as u32 + SPECIAL;
        for i in SPECIAL..n {
            match self.keys[(i - SPECIAL) as usize] {
                Key::Rel(_) => {}
                Key::Triple(_, class) => {
                    self.edges.push((i, GIVES_ALL, false));
                    self.edges.push((READS_ALL, i, false));
                    if class.is_some() {
                        self.edges.push((i, GIVES_TYPES, false));
                        self.edges.push((READS_TYPES, i, false));
                    }
                }
            }
        }
        self.edges.push((READS_ALL, READS_TYPES, false));
        self.edges.push((READS_TYPES, GIVES_TYPES, false));
        self.edges.push((GIVES_TYPES, GIVES_ALL, false));
    }

    fn strata(mut self, rules: &[Rule], vocab: &Vocab) -> Result<Strata, BudgetFailure> {
        let n = self.keys.len() + SPECIAL as usize;
        self.edges.sort_unstable_by_key(|e| e.0);
        let mut offsets = Vec::with_capacity(n + 1);
        let mut e = 0;
        for v in 0..n as u32 {
            offsets.push(e as u32);
            while e < self.edges.len() && self.edges[e].0 == v {
                e += 1;
            }
        }
        offsets.push(e as u32);
        let succ = |v: u32| {
            let (a, b) = (
                offsets[v as usize] as usize,
                offsets[v as usize + 1] as usize,
            );
            &self.edges[a..b]
        };
        let (comp, order) = tarjan(n, |v| succ(v).iter().map(|e| e.1));
        // the output, charged before it is built: the parts with their heads, the open
        // list readers and a cycle's rules
        let cycle = self
            .negative
            .iter()
            .find(|e| comp[e.0 as usize] == comp[e.1 as usize])
            .copied();
        let mut out = self.parts.len() * size_of::<Part>() + rules.len() * size_of::<usize>();
        for (_, _, heads) in &self.parts {
            if heads.spilled() {
                out += heads.len() * size_of::<usize>();
            }
        }
        if cycle.is_some() {
            out += self.parts.len() * size_of::<usize>();
        }
        let held = Held::try_new_exact(self._held.budget(), out)?;
        // the first negation on a cycle
        let stratified = match cycle {
            None => Ok(()),
            Some((h, _, ri, li)) => {
                let c = comp[h as usize];
                let mut names: Vec<usize> = Vec::with_capacity(self.parts.len());
                for (pri, nodes, _) in &self.parts {
                    if nodes.iter().any(|&x| comp[x as usize] == c) && names.last() != Some(pri) {
                        names.push(*pri);
                    }
                }
                Err(NegativeCycle {
                    rule: ri as usize,
                    literal: li as usize,
                    rules: names,
                })
            }
        };
        // a component's stratum: the most negative edges on a path below it; components
        // come in Tarjan's order, each after every component it reaches
        let mut stratum = vec![0u32; order.len()];
        for (ci, members) in order.iter().enumerate() {
            let mut s = 0;
            for &v in members {
                for e in succ(v) {
                    let d = comp[e.1 as usize] as usize;
                    if d != ci {
                        s = s.max(stratum[d] + e.2 as u32);
                    }
                }
            }
            stratum[ci] = s;
        }
        let mut parts: Vec<Part> = Vec::with_capacity(self.parts.len());
        parts.extend(self.parts.iter().map(|(ri, nodes, heads)| {
            Part {
                rule: *ri,
                heads: heads.clone(),
                stratum: nodes
                    .iter()
                    .map(|&h| stratum[comp[h as usize] as usize])
                    .max()
                    .unwrap_or(0),
            }
        }));
        let count = parts.iter().map(|p| p.stratum + 1).max().unwrap_or(1);
        let mut open_lists = Vec::with_capacity(rules.len());
        if self.gives_lists {
            for (ri, r) in rules.iter().enumerate() {
                let negative_list = r.body.iter().any(|l| {
                    matches!(l, Literal::Builtin(b) if matches!(
                        b.kind,
                        BuiltinKind::ListNotContains | BuiltinKind::ListLength | BuiltinKind::ListForAll
                    ))
                });
                if negative_list {
                    open_lists.push(ri);
                }
            }
        }
        let _ = vocab;
        Ok(Strata {
            parts,
            count,
            stratified,
            has_negation: self.has_negation,
            open_lists,
            _held: held,
        })
    }
}

/// The parts of a rule, as groups of its heads: one part with every head for a rule with
/// an existential or a `makeTemp` call, whose instances make new nodes that the heads
/// share, and one part per head otherwise. A rule without heads has one empty part.
fn groups(r: &Rule) -> Vec<SmallVec<[usize; 2]>> {
    let fresh = !r.exists.is_empty()
        || r.body
            .iter()
            .any(|l| matches!(l, Literal::Builtin(b) if b.kind == BuiltinKind::MakeTemp));
    if fresh || r.head.len() <= 1 {
        return vec![(0..r.head.len()).collect()];
    }
    (0..r.head.len())
        .map(|h| SmallVec::from_slice(&[h]))
        .collect()
}

/// Tarjan's strongly connected components without recursion: the component of each node,
/// and the components' members in the order Tarjan finds them, each after every
/// component it reaches.
pub(super) fn tarjan<I: Iterator<Item = u32>>(
    n: usize,
    succ: impl Fn(u32) -> I,
) -> (Vec<u32>, Vec<Vec<u32>>) {
    const UNSEEN: u32 = u32::MAX;
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0u32; n];
    let mut on = vec![false; n];
    let mut comp = vec![UNSEEN; n];
    let mut stack: Vec<u32> = Vec::with_capacity(n);
    let mut comps: Vec<Vec<u32>> = Vec::with_capacity(n);
    let mut next = 0u32;
    // the depth-first walk: a node and the successors it has left to visit
    let mut walk: Vec<(u32, I)> = Vec::with_capacity(n);
    for root in 0..n as u32 {
        if index[root as usize] != UNSEEN {
            continue;
        }
        index[root as usize] = next;
        low[root as usize] = next;
        next += 1;
        stack.push(root);
        on[root as usize] = true;
        walk.push((root, succ(root)));
        while let Some((v, it)) = walk.last_mut() {
            let v = *v;
            match it.next() {
                Some(w) => {
                    let wi = w as usize;
                    if index[wi] == UNSEEN {
                        index[wi] = next;
                        low[wi] = next;
                        next += 1;
                        stack.push(w);
                        on[wi] = true;
                        walk.push((w, succ(w)));
                    } else if on[wi] {
                        low[v as usize] = low[v as usize].min(index[wi]);
                    }
                }
                None => {
                    walk.pop();
                    if let Some((u, _)) = walk.last() {
                        let u = *u as usize;
                        low[u] = low[u].min(low[v as usize]);
                    }
                    if low[v as usize] == index[v as usize] {
                        let c = comps.len() as u32;
                        let mut members = Vec::new();
                        loop {
                            let w = stack.pop().expect("Tarjan's stack holds the root");
                            on[w as usize] = false;
                            comp[w as usize] = c;
                            members.push(w);
                            if w == v {
                                break;
                            }
                        }
                        comps.push(members);
                    }
                }
            }
        }
    }
    (comp, comps)
}
