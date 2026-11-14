//! Reuse of derived tables across commits (spec F17 §6.2).
//!
//! The table cache keys a table by the view it was derived from, and the view names its
//! commit. A table made at commit `c1` stays valid at a later or earlier commit `c2` of the
//! same base generation, access mask and input graphs when every quad that differs
//! between the two views, `d`, is one the rules cannot read or give:
//!
//! 1. no differing quad is a fact of the schema vocabulary (a predicate of it, or an
//!    `rdf:type` with a class of classes or properties), an RDF list fact or an
//!    `owl:sameAs` fact, and
//! 2. no differing quad unifies with an atom, a fact a builtin reads, or a head of the
//!    rules that a session of `c1`'s view made its program from, after specialization to
//!    the schema.
//!
//! Then the closure at `c2` is the closure at `c1` with the differing quads taken out and
//! put in, and no differing quad is derived at either commit. The argument: let `K` be the
//! closure at `c1` and `M` be `K` with the deleted quads out and the added ones in. A rule
//! instance whose body holds in `M` binds its schema atoms to schema facts of `M`, which
//! are those of `K` by (1), so it is an instance of a specialized rule. Each of its premises
//! is then in `K`, since an added quad would unify with that rule's atom against (2). So its
//! head is in `K`, and it is not a deleted quad, which would unify with the head. `M` is
//! therefore closed. Every derivation in `K` of a fact other than a deleted quad uses no
//! deleted quad, by the same unification, so the closure at `c2`, which `M` contains, holds
//! all of `M`. The specialized rules are those of the session's own completeness proof:
//! a relation it did not prove complete keeps its schema atoms, which are more general, so
//! (2) is only stricter. A rule left out because no `owl:sameAs` fact can hold stays out at
//! `c2` by (1). Item 191's guarantee therefore carries over: the schema facts, the proof of
//! which relations are complete, the equality stage table and every demand's table hold at
//! `c2` what they held at `c1`.
//!
//! A pattern names a constant of the rules that the store did not hold at `c1` by a local
//! id of the session, which no stored fact holds. A changed quad that holds such a term,
//! which the store gained since, is therefore relevant too. An earlier commit whose delta
//! vocabulary is shorter than `c1`'s is never answered, since `c1`'s tables may hold ids
//! of terms that commit lacks.
//!
//! A session whose view has a valid anchor reads and writes the cache under the anchor's
//! view key, so the tables, the schema, its proof and the equality table of `c1` answer
//! it. A session whose view has no anchor, or whose anchor the differing quads invalidate,
//! keys the cache by its own commit and becomes the view's anchor when it makes its
//! program. The anchor holds a clone of `c1`'s delta, whose key-set nodes it shares with
//! later deltas until writes copy them, so it retains at most the delta's size and
//! typically the nodes that later writes replaced.

use crate::builtins::BuiltinKind;
use crate::demand::{Closed, head_slots};
use crate::engine::{CRule, Slot};
use parking_lot::Mutex;
use rustc_hash::FxHashSet;
use sparkles_core::id::{Id, Tag};
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
    pub(crate) fn of(rules: &[CRule], schema: &Closed, always: &[u64], cap: usize) -> Relevance {
        let mut r = Relevance::default();
        for &p in schema.preds.iter().chain(always) {
            r.add(Slot::Any, Slot::Const(p), Slot::Any);
        }
        for &c in &schema.classes {
            r.add(Slot::Any, Slot::Const(schema.rdf_type), Slot::Const(c));
        }
        for rule in rules {
            for a in &rule.atoms {
                r.add(a.s, a.p, a.o);
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

static ANCHORS_HELD: Mutex<VecDeque<Arc<Anchor>>> = Mutex::new(VecDeque::new());

/// The cache's view key for a session over `snap` that reads the input `graphs`: the
/// anchor's, when the view has an anchor whose tables answer `snap`'s commit, else `own`.
/// `key` is the view key without the commit, with the rules' digest.
pub(crate) fn view_key(
    key: &(String, u64),
    snap: &Snapshot,
    graphs: &[Id],
    own: String,
) -> (String, bool) {
    if crate::tables::capacity() == 0 {
        return (own, false);
    }
    let anchor = {
        let held = ANCHORS_HELD.lock();
        held.iter().find(|a| a.key == *key).cloned()
    };
    let Some(a) = anchor else {
        return (own, false);
    };
    if a.commit == snap.commit {
        return (a.view.clone(), false);
    }
    if let Some(&(_, ok)) = a.memo.lock().iter().find(|(c, _)| *c == snap.commit) {
        return if ok {
            (a.view.clone(), true)
        } else {
            (own, false)
        };
    }
    let ok = answers(&a, snap, graphs);
    {
        let mut memo = a.memo.lock();
        if memo.len() >= MEMO {
            memo.pop_front();
        }
        memo.push_back((snap.commit, ok));
    }
    if ok {
        (a.view.clone(), true)
    } else {
        (own, false)
    }
}

/// Whether no quad that differs between the anchor's view and `snap`'s, in the input
/// `graphs`, can matter to the anchor's rules.
fn answers(a: &Anchor, snap: &Snapshot, graphs: &[Id]) -> bool {
    // The anchor's tables hold ids of terms its commit knew. A view whose delta
    // vocabulary is shorter, at an earlier commit, may not know them.
    if a.relevance.everything() || snap.dvocab_len < a.dvocab_len {
        return false;
    }
    let gained = a.relevance.gained(snap);
    let mut n = 0usize;
    let mut relevant = false;
    let whole = snap.delta.changes_since(&a.delta, |q| {
        if !graphs.contains(&q[3]) {
            return true;
        }
        n += 1;
        if n > MAX_CHANGES || a.relevance.reads(&[q[0].0, q[1].0, q[2].0], &gained) {
            relevant = true;
            return false;
        }
        true
    });
    whole && !relevant
}

/// Make the session over `snap` with the cache's view key `view` the anchor of its view,
/// with the patterns of the rules it made its program from.
pub(crate) fn anchor(key: (String, u64), snap: &Snapshot, view: String, relevance: Relevance) {
    if crate::tables::capacity() == 0 || relevance.everything() {
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
