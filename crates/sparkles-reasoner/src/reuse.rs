//! Reuse of derived tables across commits (spec F17 §6.2).
//!
//! The table cache keys a table by the view it was derived from, and the view names its
//! commit. A table made at commit `c1` stays valid at a later or earlier commit `c2` of the
//! same base generation, access mask and input graphs when every quad that differs
//! between the two views, `d`, is one the rules cannot read or give:
//!
//! 1. no differing quad is a fact of the schema vocabulary (a predicate of it, or an
//!    `rdf:type` with a class of classes or properties), an RDF list fact or an
//!    `owl:sameAs` fact,
//! 2. no differing quad unifies with an atom, a fact a builtin reads, or a head of the
//!    rules that a session of `c1`'s view made its program from, after specialization to
//!    the schema, or of the rules that specialization left out because their every head
//!    is a relation the session proved complete, and
//! 3. for an atom whose predicate is a variable and whose subject and object occur nowhere
//!    else in its rule, which reads only which predicates are in use, the predicate of
//!    each differing quad has a stored quad in the input graphs that is at both commits.
//!
//! Then the closure at `c2` is the closure at `c1` with the differing quads taken out and
//! put in, and no differing quad is derived at either commit. The argument: let `K` be the
//! closure at `c1` and `M` be `K` with the deleted quads out and the added ones in. A rule
//! instance whose body holds in `M` binds its schema atoms to schema facts of `M`, which
//! are those of `K` by (1), so it is an instance of a specialized rule or of a rule left
//! out. Each of its premises is then in `K`, since an added quad would unify with that
//! rule's atom against (2). An atom of (3) is the exception, and there the instance that
//! binds it to the quad (3) names instead has the same head and its premises in `K`. So
//! its head is in `K`, and it is not a deleted quad, which would unify with the head. `M`
//! is therefore closed. Every derivation in `K` of a fact other than a deleted quad uses
//! no deleted quad, by the same unification and the same exception, so the closure at
//! `c2`, which `M` contains, holds all of `M`. The specialized rules are those of the
//! session's own completeness proof: a relation it did not prove complete keeps its
//! schema atoms, which are more general, so (2) is only stricter. A rule left out because
//! no `owl:sameAs` fact can hold stays out at `c2` by (1). Item 191's guarantee therefore
//! carries over: the schema facts, the proof of which relations are complete, the
//! equality stage table and every demand's table hold at `c2` what they held at `c1`.
//!
//! A pattern names a constant of the rules that the store did not hold at `c1` by a local
//! id of the session, which no stored fact holds. A changed quad that holds such a term,
//! which the store gained since, is therefore relevant too. An earlier commit whose delta
//! vocabulary is shorter than `c1`'s is never answered, since `c1`'s tables may hold ids
//! of terms that commit lacks.
//!
//! A session whose view has a valid anchor reads and writes the cache under the anchor's
//! view key, so the tables, the schema, its proof and the equality table of `c1` answer
//! it. It keeps no table that holds a term `c1`'s delta vocabulary lacks, which a session
//! of `c1` could not read. A session whose view has no anchor, or whose anchor the differing quads invalidate,
//! keys the cache by its own commit and becomes the view's anchor when it makes its
//! program. The anchor holds a clone of `c1`'s delta, whose key-set nodes it shares with
//! later deltas until writes copy them, so it retains at most the delta's size and
//! typically the nodes that later writes replaced.

use crate::builtins::BuiltinKind;
use crate::demand::{Closed, head_slots};
use crate::engine::{CHead, CRule, Slot};
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_core::id::{Id, Tag};
use sparkles_core::index::Perm;
use sparkles_core::store::Chunk;
use sparkles_core::store::{Delta, Snapshot};
use std::collections::VecDeque;
use std::sync::Arc;

/// The most anchors kept, one per view and rules.
const ANCHORS: usize = 64;
/// The most differing quads a check reads before it gives up and calls them relevant.
pub(crate) const MAX_CHANGES: usize = 100_000;
/// The most commits an anchor remembers the answer for.
const MEMO: usize = 16;

/// The patterns of facts that a session's rules read or give, against which a differing
/// quad is tested.
#[derive(Default, Debug)]
pub(crate) struct Relevance {
    /// a pattern matches every fact, so every change is relevant
    all: bool,
    /// predicates whose every fact is relevant
    preds: FxHashSet<u64>,
    /// (predicate, subject) and (predicate, object) pairs with the other position free
    ps: FxHashSet<(u64, u64)>,
    po: FxHashSet<(u64, u64)>,
    /// patterns with every position constant
    pso: FxHashSet<[u64; 3]>,
    /// patterns with a variable predicate and a constant subject or object
    free: Vec<(Option<u64>, Option<u64>)>,
    /// an atom reads only which predicates the facts use: its predicate is a variable,
    /// and its subject and object are variables that occur nowhere else in the rule, as
    /// in `(?x ?p ?y) -> (?p rdf:type rdf:Property)`
    uses: bool,
    /// the constants of the rules that were not terms of the store, by session id, which
    /// [`Relevance::of`] collects and the anchor replaces by their keys
    locals: FxHashSet<u64>,
    /// the vocabulary keys of those constants: a term the store gains later may be one
    local_keys: Vec<Vec<u8>>,
}

impl Relevance {
    /// The patterns of `rules`, with every fact of `always` (the schema vocabulary, list
    /// facts and `owl:sameAs`) relevant. More than `cap` distinct patterns make every fact
    /// relevant, which only gives up reuse.
    pub(crate) fn of<'a>(
        rules: impl IntoIterator<Item = &'a CRule>,
        schema: &Closed,
        always: &[u64],
        cap: usize,
    ) -> Relevance {
        let mut r = Relevance::default();
        for &p in schema.preds.iter().chain(always) {
            r.add(Slot::Any, Slot::Const(p), Slot::Any);
        }
        for &c in &schema.classes {
            r.add(Slot::Any, Slot::Const(schema.rdf_type), Slot::Const(c));
        }
        for rule in rules {
            let once = singletons(rule);
            let single = |x: Slot| match x {
                Slot::Any => true,
                Slot::Var(v) => once.contains(&v),
                Slot::Const(_) => false,
            };
            for a in &rule.atoms {
                if matches!(a.p, Slot::Var(_)) && single(a.s) && single(a.o) {
                    r.uses = true;
                } else {
                    r.add(a.s, a.p, a.o);
                }
            }
            for h in &rule.head {
                // a head action gives facts of the list it walks, as its slots say
                let (s, p, o) = head_slots(h);
                r.add(s, p, o);
            }
            for b in &rule.builtins {
                let arg = |k: usize| b.args.get(k).copied().unwrap_or(Slot::Any);
                match b.kind {
                    BuiltinKind::NoValue => r.add(arg(0), arg(1), arg(2)),
                    BuiltinKind::ListForAll => r.add(arg(1), arg(2), Slot::Any),
                    _ => {}
                }
            }
            if r.all || r.len() > cap {
                return Relevance {
                    all: true,
                    ..Relevance::default()
                };
            }
        }
        r
    }

    /// Whether an atom of the rules reads only which predicates the facts use.
    pub(crate) fn uses(&self) -> bool {
        self.uses
    }

    fn len(&self) -> usize {
        self.preds.len() + self.ps.len() + self.po.len() + self.pso.len() + self.free.len()
    }

    fn add(&mut self, s: Slot, p: Slot, o: Slot) {
        let c = |x: Slot| match x {
            Slot::Const(c) => Some(c),
            _ => None,
        };
        for x in [s, p, o] {
            if let Slot::Const(c) = x
                && Id(c).tag() == Tag::Local
            {
                self.locals.insert(c);
            }
        }
        match (c(s), c(p), c(o)) {
            (_, None, None) if c(s).is_none() => self.all = true,
            (s, None, o) => self.free.push((s, o)),
            (None, Some(p), None) => {
                self.preds.insert(p);
            }
            (Some(s), Some(p), None) => {
                self.ps.insert((p, s));
            }
            (None, Some(p), Some(o)) => {
                self.po.insert((p, o));
            }
            (Some(s), Some(p), Some(o)) => {
                self.pso.insert([s, p, o]);
            }
        }
    }

    /// Give the constants that were not terms of the store their keys, with `key` the
    /// session's key of a local id. A local id without a key is a blank node a rule
    /// made, which no stored fact holds.
    pub(crate) fn key_locals<'a>(&mut self, key: impl Fn(u64) -> Option<&'a [u8]>) {
        self.local_keys = self
            .locals
            .iter()
            .filter_map(|&l| key(l))
            .map(<[u8]>::to_vec)
            .collect();
        self.locals.clear();
    }

    /// The ids in `snap` of the rules' constants that were not terms of the store at the
    /// anchor's commit and are now.
    fn gained(&self, snap: &Snapshot) -> FxHashSet<u64> {
        self.local_keys
            .iter()
            .filter_map(|k| snap.lookup_key(k))
            .map(|id| id.0)
            .collect()
    }

    /// Whether the fact `t` can matter to the rules: a pattern matches it, or it holds
    /// one of the `gained` terms, constants of the rules that the store did not hold at the
    /// anchor's commit, so that a pattern named them by a local id.
    pub(crate) fn reads(&self, t: &[u64; 3], gained: &FxHashSet<u64>) -> bool {
        if self.all {
            return true;
        }
        if !gained.is_empty() && t.iter().any(|x| gained.contains(x)) {
            return true;
        }
        let [s, p, o] = *t;
        self.preds.contains(&p)
            || self.ps.contains(&(p, s))
            || self.po.contains(&(p, o))
            || self.pso.contains(t)
            || self
                .free
                .iter()
                .any(|&(fs, fo)| fs.is_none_or(|x| x == s) && fo.is_none_or(|x| x == o))
    }

    /// Every fact is relevant, so the anchor never answers another commit.
    pub(crate) fn everything(&self) -> bool {
        self.all
    }
}

/// The view of one commit whose tables answer the commits whose differing quads the
/// rules cannot read or give.
struct Anchor {
    /// the view key without the commit, and the rules' digest
    key: (String, u64),
    commit: u64,
    /// the cache's view key of the commit, under which its tables are kept
    view: String,
    delta: Delta,
    dvocab_len: u64,
    relevance: Arc<Relevance>,
    /// commits already checked, and whether the anchor answers them
    memo: Mutex<VecDeque<(u64, bool)>>,
}

/// The variables that occur once in `rule`, in its atoms, builtins and heads together.
fn singletons(rule: &CRule) -> FxHashSet<usize> {
    let mut count: FxHashMap<usize, usize> = Default::default();
    let mut see = |x: &Slot| {
        if let Slot::Var(v) = x {
            *count.entry(*v).or_default() += 1;
        }
    };
    for a in &rule.atoms {
        [a.s, a.p, a.o].iter().for_each(&mut see);
    }
    for b in &rule.builtins {
        b.args.iter().for_each(&mut see);
    }
    for h in &rule.head {
        match h {
            CHead::Triple(s, p, o) => [*s, *p, *o].iter().for_each(&mut see),
            CHead::Action(_, args) => args.iter().for_each(&mut see),
        }
    }
    count
        .into_iter()
        .filter(|&(_, n)| n == 1)
        .map(|(v, _)| v)
        .collect()
}

static ANCHORS_HELD: Mutex<VecDeque<Arc<Anchor>>> = Mutex::new(VecDeque::new());

/// The cache's view key for a session over `snap` that reads the input `graphs`: the
/// anchor's, when the view has an anchor of another commit whose tables answer `snap`'s
/// commit, with the length of that commit's delta vocabulary, else `own` and `None`.
/// `key` is the view key without the commit, with the rules' digest. `stop` is the
/// query's stop check, which the walk over the differing quads calls.
pub(crate) fn view_key(
    key: &(String, u64),
    snap: &Snapshot,
    graphs: &[Id],
    own: String,
    stop: &dyn Fn() -> sparkles_core::Result<()>,
) -> (String, Option<u64>) {
    if crate::tables::capacity() == 0 {
        return (own, None);
    }
    let anchor = {
        let held = ANCHORS_HELD.lock();
        held.iter().find(|a| a.key == *key).cloned()
    };
    let Some(a) = anchor else {
        return (own, None);
    };
    if a.commit == snap.commit {
        return (own, None);
    }
    let memo = a
        .memo
        .lock()
        .iter()
        .find(|(c, _)| *c == snap.commit)
        .map(|&(_, ok)| ok);
    let ok = match memo {
        Some(ok) => ok,
        None => {
            let ok = answers(&a, snap, graphs, stop);
            let mut memo = a.memo.lock();
            if memo.len() >= MEMO {
                memo.pop_front();
            }
            memo.push_back((snap.commit, ok));
            ok
        }
    };
    if ok {
        (a.view.clone(), Some(a.dvocab_len))
    } else {
        (own, None)
    }
}

/// Whether no quad that differs between the anchor's view and `snap`'s, in the input
/// `graphs`, can matter to the anchor's rules.
fn answers(
    a: &Anchor,
    snap: &Snapshot,
    graphs: &[Id],
    stop: &dyn Fn() -> sparkles_core::Result<()>,
) -> bool {
    // The anchor's tables hold ids of terms its commit knew. A view whose delta
    // vocabulary is shorter, at an earlier commit, may not know them.
    if a.relevance.everything() || snap.dvocab_len < a.dvocab_len {
        return false;
    }
    let gained = a.relevance.gained(snap);
    let mut n = 0usize;
    let mut relevant = false;
    // the differing quads in the input graphs by predicate, when an atom reads which
    // predicates are in use
    let mut used: FxHashMap<u64, FxHashSet<[u64; 4]>> = FxHashMap::default();
    let whole = snap.delta.changes_since(&a.delta, |q| {
        // every differing quad counts toward the walk's bound, in the input graphs or not
        n += 1;
        if n > MAX_CHANGES || (n.is_multiple_of(4096) && stop().is_err()) {
            relevant = true;
            return false;
        }
        if !graphs.contains(&q[3]) {
            return true;
        }
        if a.relevance.reads(&[q[0].0, q[1].0, q[2].0], &gained) {
            relevant = true;
            return false;
        }
        if a.relevance.uses() {
            // an access mask hides quads that the check below would count
            if snap.mask.is_some() {
                relevant = true;
                return false;
            }
            used.entry(q[1].0)
                .or_default()
                .insert([q[0].0, q[1].0, q[2].0, q[3].0]);
        }
        true
    });
    if !whole || relevant {
        return false;
    }
    // A predicate of a differing quad is in use at both commits when a quad of it in the
    // input graphs is at both, one that is not a differing quad.
    used.iter()
        .all(|(&p, differing)| stop().is_ok() && kept_in_use(snap, graphs, p, differing))
}

/// Whether `snap` holds a quad of `p` in `graphs` that is not one of `differing`, among
/// the first quads of `p`. Not finding one within the bound only gives up reuse.
fn kept_in_use(snap: &Snapshot, graphs: &[Id], p: u64, differing: &FxHashSet<[u64; 4]>) -> bool {
    const BOUND: usize = 4096;
    let mut seen = 0usize;
    let mut found = false;
    let mut row = |k: [u64; 4]| {
        seen += 1;
        let q = Perm::Pso.to_quad(&k);
        if graphs.contains(&q[3]) && !differing.contains(&[q[0].0, q[1].0, q[2].0, q[3].0]) {
            found = true;
        }
        !found && seen < BOUND
    };
    let scanned = snap.scan(Perm::Pso, &[p], |c| {
        Ok(match c {
            Chunk::Block(b, s, e) => (s..e).all(|i| row(b.key(i))),
            Chunk::Row(k) => row(k),
        })
    });
    scanned.is_ok() && found
}

/// Make the session over `snap` with the cache's view key `view` the anchor of its view,
/// with the patterns of the rules it made its program from.
pub(crate) fn anchor(key: (String, u64), snap: &Snapshot, view: String, relevance: Relevance) {
    if relevance.everything() {
        return;
    }
    let a = Arc::new(Anchor {
        key,
        commit: snap.commit,
        view,
        delta: snap.delta.clone(),
        dvocab_len: snap.dvocab_len,
        relevance: Arc::new(relevance),
        memo: Mutex::new(VecDeque::new()),
    });
    let mut held = ANCHORS_HELD.lock();
    // checked under the lock, so that an anchor made while the cache is turned off does
    // not outlive [`clear`]
    if crate::tables::capacity() == 0 {
        return;
    }
    held.retain(|x| x.key != a.key);
    if held.len() >= ANCHORS {
        held.pop_front();
    }
    held.push_back(a);
}

/// Drop every anchor, as [`crate::tables::clear`] drops the tables.
pub(crate) fn clear() {
    ANCHORS_HELD.lock().clear();
}
