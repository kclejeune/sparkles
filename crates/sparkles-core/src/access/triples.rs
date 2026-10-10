//! Triple-level access: protections that hide some triples of the graphs a view reads,
//! and keep a caller from writing them (spec C12, Phase 2).
//!
//! A [`Protection`] matches triples by predicate, by the class of their subject, and by
//! graph. A caller sees a matched triple only when one of its grants lifts the
//! protection in the triple's graph, or when the protection's pattern matches the
//! triple with the caller's attributes bound. Every protection that matches a triple
//! must be lifted, so protections combine like deny rules that override, while the
//! grants that lift them form a union. Neither depends on the order of the rules.
//!
//! Rules are never evaluated per triple during a query. For a snapshot and a view, the
//! quads the view hides are worked out once, as sets: the quads of the protected
//! predicates, the quads of the subjects of the protected classes, and the subjects (or
//! triples) a pattern lets through. The hidden quads are then applied to the snapshot
//! as extra deletions ([`masked`]). The masked snapshot is an ordinary state of the
//! store, so every scan, count, statistics correction, path, search and diff of the
//! engine reads the visible triples only, with no operator knowing about the rules.

use super::{GraphAccess, Graphs, glob, graph_term};
use crate::error::{Budget, BudgetKind, Error, Result};
use crate::id::Id;
use crate::index::{Key, Perm};
use crate::store::{Chunk, Snapshot};
use oxrdf::Term;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The graph of materialized inferences.
pub const INFERRED_GRAPH: &str = "urn:x-sparkles:inferred";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// Triples that only some callers may read or write.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Protection {
    /// the name grants lift it by
    pub name: String,
    /// predicate IRIs and IRI patterns with `*` (`None`: every predicate)
    pub predicates: Option<Vec<String>>,
    /// subject classes (`None`: every subject)
    pub classes: Option<Vec<String>>,
    /// a subject of a subclass (through `rdfs:subClassOf` in any graph) counts
    pub subclasses: bool,
    /// the graphs it applies in (`None`: every graph)
    pub graphs: Option<Graphs>,
    /// a SPARQL group graph pattern: a matched triple passes when the pattern has a
    /// solution for it (its `?s` or `?this`, `?p` and `?o`, those it uses) with the
    /// caller bound to `?user`, `?role` and `?group`
    pub pattern: Option<String>,
    /// prefixes of the pattern
    pub prefixes: BTreeMap<String, String>,
    /// while it is in force for a caller, the caller does not read the graph of
    /// materialized inferences, which may restate what it hides
    pub hide_inferences: bool,
}

impl Protection {
    /// Whether the predicate `iri` is in the protection's scope.
    pub fn covers_predicate(&self, iri: &str) -> bool {
        self.predicates.as_ref().is_none_or(|ps| {
            ps.iter()
                .any(|p| p == iri || p.contains('*') && glob(p, iri))
        })
    }

    /// Whether the graph named by `g` (`None`: the default graph) is in the scope.
    pub fn covers_graph(&self, g: Option<&Term>) -> bool {
        self.graphs.as_ref().is_none_or(|gs| gs.allows(g))
    }
}

/// The attributes of a caller that patterns may use.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Caller {
    /// bound to `?user` (unbound for anonymous callers)
    pub user: Option<String>,
    /// each one is a value of `?role`
    pub roles: Vec<String>,
    /// each one is a value of `?group`
    pub groups: Vec<String>,
}

/// A protection as it applies to one caller.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Rule {
    pub protection: Arc<Protection>,
    /// the graphs where the caller's grants lift it for reading
    pub read: Graphs,
    /// the graphs where the caller's grants lift it for writing
    pub write: Graphs,
}

/// Limits on the work of applying rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Limits {
    /// the most quads a view may hide at one commit
    pub max_hidden: u64,
    /// the most solutions a protection's pattern may have
    pub max_pattern_rows: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_hidden: 5_000_000,
            max_pattern_rows: 1_000_000,
        }
    }
}

/// The protections of one view, with the caller they apply to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TripleRules {
    pub rules: Vec<Rule>,
    pub caller: Caller,
    pub limits: Limits,
}

impl TripleRules {
    /// The rules that some graph does not lift, for reading or for writing.
    pub fn in_force(mut self) -> TripleRules {
        self.rules.retain(|r| !r.read.is_all() || !r.write.is_all());
        self
    }

    /// Whether some rule hides triples from reads.
    pub fn hides(&self) -> bool {
        self.rules.iter().any(|r| !r.read.is_all())
    }

    /// Whether some rule limits writes.
    pub fn limits_writes(&self) -> bool {
        self.rules.iter().any(|r| !r.write.is_all())
    }

    /// Whether a rule in force for reads hides the graph of inferences.
    pub fn hides_inferences(&self) -> bool {
        self.rules
            .iter()
            .any(|r| !r.read.is_all() && r.protection.hide_inferences)
    }

    /// Whether the rules in force for reads match triples by predicate and graph
    /// only, so that whether a quad is hidden depends on the quad alone and not on the
    /// state of the store.
    pub fn state_independent(&self) -> bool {
        self.reads()
            .all(|r| r.protection.classes.is_none() && r.protection.pattern.is_none())
    }

    /// For [`state_independent`](Self::state_independent) rules: whether a quad with the
    /// predicate `pred` in the graph `g` (`None`: the default graph) is hidden.
    pub fn hides_quad(&self, pred: &str, g: Option<&Term>) -> bool {
        self.reads().any(|r| {
            r.protection.covers_predicate(pred) && r.protection.covers_graph(g) && !r.read.allows(g)
        })
    }

    fn reads(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter().filter(|r| !r.read.is_all())
    }

    /// A key of what the rules hide from reads: equal keys hide the same quads of a
    /// snapshot. The caller is part of it only when a pattern may use it.
    pub fn read_key(&self) -> String {
        let rules: Vec<(&Protection, &Graphs)> =
            self.reads().map(|r| (&*r.protection, &r.read)).collect();
        if rules.iter().any(|(p, _)| p.pattern.is_some()) {
            format!("{rules:?}|{:?}|{:?}", self.caller, self.limits)
        } else {
            format!("{rules:?}|{:?}", self.limits)
        }
    }
}

/// The quads a view hides at one commit: what a masked snapshot leaves out.
#[derive(Debug)]
pub struct Mask {
    /// unique in the process (caches keyed by a snapshot's identity tell masks apart)
    pub id: u64,
    /// the view's key: two masks of one commit with the same key hide the same quads
    pub key: String,
    /// the hidden quads as SPO keys, sorted
    pub hidden: Vec<Key>,
}

impl Mask {
    /// Whether the quad `[s, p, o, g]` is hidden.
    pub fn hides(&self, q: &[u64; 4]) -> bool {
        self.hidden.binary_search(q).is_ok()
    }
}

static MASK_IDS: AtomicU64 = AtomicU64::new(1);

/// `snap` with the quads the view hides taken out, kept with `snap` per view.
pub(super) fn masked(
    access: &GraphAccess,
    rules: &TripleRules,
    snap: &Arc<Snapshot>,
) -> Result<Arc<Snapshot>> {
    let key = format!("{:?}|{}", access.read, rules.read_key());
    let slot = snap.counts.mask_slot(&key);
    let mut held = slot.lock();
    if let Some(m) = &*held {
        return Ok(m.clone());
    }
    let hidden = hidden_quads(access, rules, snap)?;
    let m = Arc::new(apply(snap, hidden, key));
    *held = Some(m.clone());
    Ok(m)
}

/// `snap` with the quads `hidden` (SPO keys, sorted, all present in `snap`) taken out:
/// those of the base become deletions, those the delta inserted leave it, and the delta's
/// predicate statistics follow. The counts and the caches keyed by the snapshot start
/// empty.
pub fn apply(snap: &Snapshot, hidden: Vec<Key>, key: String) -> Snapshot {
    use rayon::prelude::*;
    let spo = Perm::Spo.index();
    let (from_ins, from_base): (Vec<Key>, Vec<Key>) =
        hidden.iter().partition(|k| snap.delta.ins[spo].contains(k));
    let dels: Vec<_> = Perm::ALL
        .par_iter()
        .map(|&p| {
            let mut del = snap.delta.del[p.index()].clone();
            for k in &from_base {
                del.insert(p.to_key(&Perm::Spo.to_quad(k)));
            }
            del
        })
        .collect();
    let mut delta = snap.delta.clone();
    for (i, del) in dels.into_iter().enumerate() {
        delta.del[i] = del;
    }
    let from_ins: Vec<[Id; 4]> = from_ins.iter().map(|k| Perm::Spo.to_quad(k)).collect();
    delta.remove_inserted_all(&from_ins);
    Snapshot {
        delta,
        counts: Default::default(),
        mask: Some(Arc::new(Mask {
            id: MASK_IDS.fetch_add(1, Ordering::Relaxed),
            key,
            hidden,
        })),
        ..snap.clone()
    }
}

/// What the graphs of quads are, worked out once per graph id.
struct GraphInfo<'a> {
    snap: &'a Snapshot,
    memo: FxHashMap<u64, Option<Term>>,
}

impl<'a> GraphInfo<'a> {
    fn new(snap: &'a Snapshot) -> GraphInfo<'a> {
        GraphInfo {
            snap,
            memo: Default::default(),
        }
    }

    /// The term naming graph `g` (`None` for the default graph), `Err(())` for an id
    /// that names no term.
    fn term(&mut self, g: u64) -> std::result::Result<Option<&Term>, ()> {
        if g == Id::DEFAULT_GRAPH.0 {
            return Ok(None);
        }
        let snap = self.snap;
        self.memo
            .entry(g)
            .or_insert_with(|| graph_term(snap, Id(g)))
            .as_ref()
            .map(Some)
            .ok_or(())
    }
}

/// The solutions of a protection's pattern, as the keys a triple is looked up by.
#[derive(Debug)]
pub(crate) enum Passes {
    /// the pattern uses none of `?s`, `?p` and `?o`, and has a solution
    All,
    /// the same, without a solution
    None,
    /// the `?s` (or `?this`), `?p` and `?o` values of the solutions, where `cols` says
    /// which of the three the pattern binds (the others are 0)
    Keys {
        cols: [bool; 3],
        set: FxHashSet<[u64; 3]>,
    },
}

impl Passes {
    pub(crate) fn passes(&self, q: &[u64; 4]) -> bool {
        match self {
            Passes::All => true,
            Passes::None => false,
            Passes::Keys { cols, set } => {
                let k = [
                    if cols[0] { q[0] } else { 0 },
                    if cols[1] { q[1] } else { 0 },
                    if cols[2] { q[2] } else { 0 },
                ];
                set.contains(&k)
            }
        }
    }
}

/// The variables of a pattern among `vars` (by a scan of its text: a name in a string
/// only adds a binding).
fn uses(pattern: &str, var: &str) -> bool {
    let b = pattern.as_bytes();
    let mut i = 0;
    while let Some(j) = pattern[i..].find(var) {
        let at = i + j;
        let before = at.checked_sub(1).map(|k| b[k]);
        let after = b.get(at + var.len()).copied();
        if matches!(before, Some(b'?' | b'$'))
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            return true;
        }
        i = at + var.len();
    }
    false
}

/// The query that evaluates a protection's pattern for a caller, and which of `?s`,
/// `?p` and `?o` it projects (the subject's variable is `?this` when the pattern uses
/// it). `None` when the caller lacks an attribute the pattern uses (it matches nothing).
pub fn pattern_query(
    p: &Protection,
    pattern: &str,
    caller: &Caller,
) -> Option<(String, [bool; 3])> {
    let lit = |v: &str| oxrdf::Literal::new_simple_literal(v).to_string();
    let mut q = String::new();
    for (k, v) in &p.prefixes {
        q.push_str(&format!("PREFIX {k}: <{v}>\n"));
    }
    let subject = if uses(pattern, "this") { "this" } else { "s" };
    let cols = [
        uses(pattern, subject),
        uses(pattern, "p"),
        uses(pattern, "o"),
    ];
    let mut proj = String::new();
    for (on, v) in cols.iter().zip([subject, "p", "o"]) {
        if *on {
            proj.push_str(&format!(" ?{v}"));
        }
    }
    if proj.is_empty() {
        proj.push_str(" (1 AS ?any)");
    }
    q.push_str(&format!("SELECT DISTINCT{proj} WHERE {{\n"));
    let mut values = |var: &str, vals: &[String]| -> bool {
        if !uses(pattern, var) {
            return true;
        }
        if vals.is_empty() {
            return false;
        }
        let vs: Vec<String> = vals.iter().map(|v| lit(v)).collect();
        q.push_str(&format!("  VALUES ?{var} {{ {} }}\n", vs.join(" ")));
        true
    };
    let user: Vec<String> = caller.user.iter().cloned().collect();
    if !values("user", &user) || !values("role", &caller.roles) || !values("group", &caller.groups)
    {
        return None;
    }
    q.push_str(&format!("  {{ {pattern}\n  }}\n}}"));
    Some((q, cols))
}

/// Evaluate a protection's pattern on `snap` (every graph merged into the default
/// graph, `GRAPH` for the named ones), within the limits.
pub(crate) fn passes(
    snap: &Arc<Snapshot>,
    p: &Protection,
    caller: &Caller,
    limits: &Limits,
) -> Result<Passes> {
    let Some(pattern) = &p.pattern else {
        return Ok(Passes::None);
    };
    let Some((q, cols)) = pattern_query(p, pattern, caller) else {
        return Ok(Passes::None);
    };
    let named: Vec<String> = snap
        .graph_ids()?
        .into_iter()
        .filter_map(|g| match snap.term(g) {
            Some(Term::NamedNode(n)) => Some(n.into_string()),
            _ => None,
        })
        .collect();
    let opts = crate::sparql::QueryOptions {
        default_graph_extra: named,
        max_rows: Some(usize::try_from(limits.max_pattern_rows).unwrap_or(usize::MAX)),
        ..Default::default()
    };
    let r = crate::sparql::query(snap.clone(), &q, &opts).map_err(|e| match e {
        Error::BudgetExceeded(_) => Error::BudgetExceeded(Budget {
            kind: BudgetKind::Rows,
            limit: limits.max_pattern_rows,
            requested: limits.max_pattern_rows.saturating_add(1),
        }),
        Error::SparqlSyntax(e) => {
            Error::invalid(format!("the pattern of protection '{}': {e}", p.name))
        }
        e => e,
    })?;
    if r.table.len() as u64 > limits.max_pattern_rows {
        return Err(Error::BudgetExceeded(Budget {
            kind: BudgetKind::Rows,
            limit: limits.max_pattern_rows,
            requested: r.table.len() as u64,
        }));
    }
    if !cols.iter().any(|c| *c) {
        return Ok(if r.table.is_empty() {
            Passes::None
        } else {
            Passes::All
        });
    }
    let mut set = FxHashSet::default();
    let n = cols.iter().filter(|c| **c).count();
    for i in 0..r.table.len() {
        let mut k = [0u64; 3];
        let mut c = 0;
        for (j, on) in cols.iter().enumerate() {
            if *on {
                k[j] = r.table.cols[c][i].0;
                c += 1;
            }
        }
        debug_assert_eq!(c, n);
        set.insert(k);
    }
    Ok(Passes::Keys { cols, set })
}

/// The classes `classes` and, with `subclasses`, every class below them through
/// `rdfs:subClassOf` in any graph of `snap` (ids of stored terms).
pub(crate) fn class_closure(
    snap: &Snapshot,
    classes: &[String],
    subclasses: bool,
) -> Result<FxHashSet<u64>> {
    let mut out: FxHashSet<u64> = classes
        .iter()
        .filter_map(|c| snap.lookup_iri(c))
        .map(|id| id.0)
        .collect();
    if !subclasses {
        return Ok(out);
    }
    let Some(sub) = snap.lookup_iri(SUB_CLASS_OF) else {
        return Ok(out);
    };
    let mut frontier: Vec<u64> = out.iter().copied().collect();
    while let Some(c) = frontier.pop() {
        for k in snap.scan_keys(Perm::Pos, &[sub.0, c])? {
            let s = Perm::Pos.to_quad(&k)[0].0;
            if out.insert(s) {
                frontier.push(s);
            }
        }
    }
    Ok(out)
}

/// The subjects with an `rdf:type` in `closure`, in any graph of `snap`, sorted.
fn class_members(snap: &Snapshot, closure: &FxHashSet<u64>) -> Result<Vec<u64>> {
    let Some(t) = snap.lookup_iri(RDF_TYPE) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for &c in closure {
        for k in snap.scan_keys(Perm::Pos, &[t.0, c])? {
            out.push(Perm::Pos.to_quad(&k)[0].0);
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// The predicate ids of `snap` in a protection's predicate scope, sorted (`None`: every
/// predicate).
fn predicate_ids(snap: &Snapshot, p: &Protection) -> Result<Option<Vec<u64>>> {
    let Some(ps) = &p.predicates else {
        return Ok(None);
    };
    let mut out: Vec<u64> = ps
        .iter()
        .filter(|x| !x.contains('*'))
        .filter_map(|x| snap.lookup_iri(x))
        .map(|id| id.0)
        .collect();
    if ps.iter().any(|x| x.contains('*')) {
        for pid in snap.distinct_first(Perm::Pso)? {
            if let Some(Term::NamedNode(n)) = snap.term(Id(pid))
                && p.covers_predicate(n.as_str())
            {
                out.push(pid);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(Some(out))
}

/// Visit the quads of `snap` whose key starts with `prefix` in `perm`, as quads.
fn each_quad(
    snap: &Snapshot,
    perm: Perm,
    prefix: &[u64],
    f: &mut dyn FnMut([u64; 4]) -> Result<()>,
) -> Result<()> {
    snap.scan(perm, prefix, |c| {
        match c {
            Chunk::Block(b, s, e) => {
                for i in s..e {
                    let q = perm.to_quad(&b.key(i));
                    f([q[0].0, q[1].0, q[2].0, q[3].0])?;
                }
            }
            Chunk::Row(k) => {
                let q = perm.to_quad(&k);
                f([q[0].0, q[1].0, q[2].0, q[3].0])?;
            }
        }
        Ok(true)
    })
}

fn too_many(limits: &Limits, n: u64) -> Error {
    Error::BudgetExceeded(Budget {
        kind: BudgetKind::HiddenQuads,
        limit: limits.max_hidden,
        requested: n,
    })
}

/// The quads of `snap` the view hides: in a graph it reads, matched by a rule in force
/// for reads that the caller's grants do not lift in that graph and whose pattern
/// does not let the triple through. SPO keys, sorted.
fn hidden_quads(
    access: &GraphAccess,
    rules: &TripleRules,
    snap: &Arc<Snapshot>,
) -> Result<Vec<Key>> {
    let mut hidden: Vec<Key> = Vec::new();
    let mut graphs = GraphInfo::new(snap);
    // whether each graph is read at all (quads of other graphs need no mask)
    let mut read: FxHashMap<u64, bool> = Default::default();
    for rule in rules.reads() {
        let p = &rule.protection;
        let preds = predicate_ids(snap, p)?;
        let members = match &p.classes {
            Some(cs) => Some(class_members(
                snap,
                &class_closure(snap, cs, p.subclasses)?,
            )?),
            None => None,
        };
        let pass = match &p.pattern {
            Some(_) => Some(passes(snap, p, &rules.caller, &rules.limits)?),
            None => None,
        };
        // (graph id → hidden by this rule in that graph, before the pattern)
        let mut by_graph: FxHashMap<u64, bool> = Default::default();
        let mut visit = |q: [u64; 4]| -> Result<()> {
            let g = q[3];
            let hide = match by_graph.get(&g) {
                Some(h) => *h,
                None => {
                    let readable = *read
                        .entry(g)
                        .or_insert_with(|| access.readable_id(snap, Id(g)));
                    let h = readable
                        && match graphs.term(g) {
                            Ok(t) => p.covers_graph(t) && !rule.read.allows(t),
                            // an id that names no graph: never in a view
                            Err(()) => false,
                        };
                    by_graph.insert(g, h);
                    h
                }
            };
            if hide && !pass.as_ref().is_some_and(|ps| ps.passes(&q)) {
                hidden.push(q);
                if hidden.len() as u64 > rules.limits.max_hidden {
                    return Err(too_many(&rules.limits, hidden.len() as u64));
                }
            }
            Ok(())
        };
        match (&members, &preds) {
            (Some(m), preds) => {
                for &s in m {
                    each_quad(snap, Perm::Spo, &[s], &mut |q| {
                        if preds
                            .as_ref()
                            .is_none_or(|ps| ps.binary_search(&q[1]).is_ok())
                        {
                            visit(q)?;
                        }
                        Ok(())
                    })?;
                }
            }
            (None, Some(ps)) => {
                for &pid in ps {
                    each_quad(snap, Perm::Pso, &[pid], &mut visit)?;
                }
            }
            (None, None) => each_quad(snap, Perm::Gspo, &[], &mut visit)?,
        }
    }
    hidden.sort_unstable();
    hidden.dedup();
    Ok(hidden)
}

/// The rules of a view applied to writes at one state of the store: the protection of
/// each quad a write asks to insert or delete. Class members and pattern solutions are
/// looked up for the quads asked about only.
pub struct WriteCheck {
    rules: Arc<TripleRules>,
    snap: Arc<Snapshot>,
    rdf_type: Option<u64>,
    sub_class_of: Option<u64>,
    /// per rule: the class closure (rules with classes)
    closures: Vec<Option<FxHashSet<u64>>>,
    /// per rule: the pattern's solutions, worked out on first use
    passes: Vec<Option<Passes>>,
    /// (rule, subject) → member of the rule's classes
    members: FxHashMap<(usize, u64), bool>,
    /// predicate id → IRI
    preds: FxHashMap<u64, Option<String>>,
    graphs: FxHashMap<u64, Option<Term>>,
}

impl WriteCheck {
    /// The check of `rules` at the state `snap` (`None` when no rule limits writes).
    pub fn new(rules: &Arc<TripleRules>, snap: Arc<Snapshot>) -> Result<Option<WriteCheck>> {
        if !rules.limits_writes() {
            return Ok(None);
        }
        let mut closures = Vec::with_capacity(rules.rules.len());
        for r in &rules.rules {
            closures.push(match &r.protection.classes {
                Some(cs) if !r.write.is_all() => {
                    Some(class_closure(&snap, cs, r.protection.subclasses)?)
                }
                _ => None,
            });
        }
        Ok(Some(WriteCheck {
            rules: rules.clone(),
            rdf_type: snap.lookup_iri(RDF_TYPE).map(|i| i.0),
            sub_class_of: snap.lookup_iri(SUB_CLASS_OF).map(|i| i.0),
            passes: (0..rules.rules.len()).map(|_| None).collect(),
            closures,
            members: Default::default(),
            preds: Default::default(),
            graphs: Default::default(),
            snap,
        }))
    }

    fn is_member(&mut self, rule: usize, s: u64) -> Result<bool> {
        if let Some(m) = self.members.get(&(rule, s)) {
            return Ok(*m);
        }
        let (Some(t), Some(closure)) = (self.rdf_type, self.closures[rule].as_ref()) else {
            return Ok(false);
        };
        let mut m = false;
        for k in self.snap.scan_keys(Perm::Spo, &[s, t])? {
            if closure.contains(&k[2]) {
                m = true;
                break;
            }
        }
        self.members.insert((rule, s), m);
        Ok(m)
    }

    /// [`Error::NotPermitted`] unless the quad `q` (ids of this state, `Id::UNDEF` for
    /// a term the store does not have) may be written. `pred` is the predicate's IRI.
    /// The answer depends on the quad's terms and on the subject's classes and the
    /// patterns at this state, never on whether the quad itself exists.
    pub fn check(&mut self, q: [Id; 4], pred: &str, graph: Option<&Term>) -> Result<()> {
        let rules = self.rules.clone();
        for (i, r) in rules.rules.iter().enumerate() {
            if r.write.is_all() {
                continue;
            }
            let p = &r.protection;
            let s = q[0].0;
            let matched = if p.classes.is_some() {
                // changing the class hierarchy below a protected class changes who is
                // protected: it needs the protection lifted too
                let edge = self.sub_class_of.is_some_and(|sc| q[1].0 == sc)
                    && self.closures[i]
                        .as_ref()
                        .is_some_and(|c| c.contains(&q[0].0) || c.contains(&q[2].0));
                edge || (p.covers_predicate(pred)
                    && p.covers_graph(graph)
                    && !q[0].is_undef()
                    && self.is_member(i, s)?)
            } else {
                p.covers_predicate(pred) && p.covers_graph(graph)
            };
            if !matched || r.write.allows(graph) {
                continue;
            }
            if p.pattern.is_some() && !q.iter().take(3).any(|x| x.is_undef()) {
                if self.passes[i].is_none() {
                    self.passes[i] = Some(passes(&self.snap, p, &rules.caller, &rules.limits)?);
                }
                if self.passes[i]
                    .as_ref()
                    .is_some_and(|ps| ps.passes(&[q[0].0, q[1].0, q[2].0, q[3].0]))
                {
                    continue;
                }
            }
            return Err(refused(&self.snap, q, pred));
        }
        Ok(())
    }

    /// [`check`](Self::check) for a quad of stored ids.
    pub fn check_ids(&mut self, q: [Id; 4]) -> Result<()> {
        let pred = match self.preds.get(&q[1].0) {
            Some(p) => p.clone(),
            None => {
                let p = match self.snap.term(q[1]) {
                    Some(Term::NamedNode(n)) => Some(n.into_string()),
                    _ => None,
                };
                self.preds.insert(q[1].0, p.clone());
                p
            }
        };
        let Some(pred) = pred else {
            return Ok(());
        };
        let graph = if q[3] == Id::DEFAULT_GRAPH {
            None
        } else {
            let snap = &self.snap;
            match self
                .graphs
                .entry(q[3].0)
                .or_insert_with(|| graph_term(snap, q[3]))
            {
                Some(t) => Some(t.clone()),
                None => return Ok(()),
            }
        };
        self.check(q, &pred, graph.as_ref())
    }
}

/// The refusal of a write of a protected triple. It names the triple the caller asked
/// for, never the protection, whose name or classes could tell more.
fn refused(snap: &Snapshot, q: [Id; 4], pred: &str) -> Error {
    let t = |id: Id| {
        snap.term(id)
            .map_or_else(|| "…".to_string(), |t| t.to_string())
    };
    Error::NotPermitted(format!(
        "write access to the triple {} <{pred}> {} required",
        t(q[0]),
        t(q[2])
    ))
}
