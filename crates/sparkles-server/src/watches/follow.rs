//! The follower: reads each watched branch's commits in order and matches them.
//!
//! One thread serves every branch. For each branch with a watch it keeps a cursor, a
//! delta subscription and, for each graph with a subtree watch, the graph's asserted
//! hierarchy as a map from child to parents. The map is kept in IRIs, which survive
//! the store's generation changes, and copied into ids of the current generation for
//! the walks. It is loaded once from the state at the cursor and then kept current
//! from each commit's hierarchy changes, in commit order, so the state before and after
//! each commit is known exactly.
//!
//! A changed quad in a watched graph matches the entity watches whose entity it names.
//! For subtree watches, each IRI it names is walked upward through its parents, in the
//! hierarchy before the commit and, when the commit changed the hierarchy, after it,
//! and the walk matches every subtree watch whose entity it reaches within the watch's
//! depth. The cost of a commit is its changes times the ancestors each walk visits,
//! whatever the size of a subtree.

use super::{MATCH_MEMBERS, MAX_GRAPH_EDGES, Match, Owner, Scope, Watch, now_text};
use crate::auth::Kind;
use crate::notify::config::WatchSettings;
use crate::state::AppState;
use sparkles::history::{At, HistoryOptions};
use sparkles::id::Id;
use sparkles::index::Perm;
use sparkles::store::{
    ChangesOptions, Chunk, CommitDelta, DeltaSubscription, Snapshot, Store, TermKind,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Weak};
use std::time::Duration;

/// How often the follower looks at the branches when nothing wakes it.
const TICK: Duration = Duration::from_secs(1);
/// The commits one page of deltas holds.
const PAGE: usize = 64;
/// The pages one branch reads before the follower turns to the next branch.
const PAGES: usize = 16;

pub const SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
pub const SUBPROPERTY: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
pub const BROADER: &str = "http://www.w3.org/2004/02/skos/core#broader";
pub const NARROWER: &str = "http://www.w3.org/2004/02/skos/core#narrower";
/// The hierarchy predicates, and whether the subject is the child.
const HIERARCHY: [(&str, bool); 4] = [
    (SUBCLASS, true),
    (SUBPROPERTY, true),
    (BROADER, true),
    (NARROWER, false),
];

/// Start the follower thread, which ends once the server is gone.
pub fn spawn(weak: Weak<AppState>) {
    let r = std::thread::Builder::new()
        .name("sparkles-watches".into())
        .spawn(move || {
            let mut f = Followers::default();
            loop {
                let Some(st) = weak.upgrade() else { return };
                let v = st.watches.version();
                f.step(&st);
                super::deliver::flush(&st);
                st.watches.wait(v, TICK);
            }
        });
    if let Err(e) = r {
        tracing::error!(target: "sparkles::watches", "cannot start the watch follower: {e}");
    }
}

/// The followers of every watched branch.
#[derive(Default)]
pub struct Followers {
    by_branch: HashMap<uuid::Uuid, Follower>,
}

impl Followers {
    /// The names every follower keeps (tests).
    #[cfg(test)]
    pub(crate) fn names(&self) -> usize {
        self.by_branch.values().map(Follower::names).sum()
    }

    /// Read every watched branch's new commits and record the matches.
    pub fn step(&mut self, st: &Arc<AppState>) {
        let Ok((watches, cursors, version)) = st.watches.snapshot(st) else {
            self.by_branch.clear();
            return;
        };
        let bounds = crate::notify::settings(st).watches;
        let mut groups: BTreeMap<uuid::Uuid, Vec<Watch>> = BTreeMap::new();
        for w in watches {
            groups.entry(w.branch_id).or_default().push(w);
        }
        self.by_branch.retain(|id, _| groups.contains_key(id));
        let mut gone: Vec<uuid::Uuid> = Vec::new();
        for (bid, ws) in groups {
            let (main, name) = match locate(st, &ws[0].dataset, bid) {
                Place::Found(d, n) => (d, n),
                Place::Unknown => continue,
                Place::Gone => {
                    gone.push(bid);
                    continue;
                }
            };
            let dataset = main.name.clone();
            let ds = match st.branch_dataset(&main, &name) {
                Ok(ds) if ds.store.branch_id() == bid => ds,
                _ => continue,
            };
            let cursor = cursors
                .get(&bid)
                .copied()
                .unwrap_or_else(|| ws.iter().map(|w| w.since).min().unwrap_or(0));
            let f = self
                .by_branch
                .entry(bid)
                .or_insert_with(|| Follower::new(cursor));
            if f.bloated() {
                // names of hierarchy nodes and watches that are gone: start again from
                // the follower's cursor, reading the hierarchy there
                *f = Follower::new(f.cursor);
            }
            if f.version != version {
                f.sync(ws, version, cursor);
            }
            let before = f.cursor;
            let out = f.run(st, &ds.store, &dataset, &name, &bounds);
            let known: BTreeSet<String> = f.watches.iter().map(|w| w.id.clone()).collect();
            let saved = st.watches.record(
                st,
                bid,
                out.cursor,
                out.found,
                &out.truncated,
                (&dataset, &name),
                &known,
            );
            if !saved {
                f.rewind(before);
            }
        }
        if !gone.is_empty() {
            st.watches.remove_where(st, |w| gone.contains(&w.branch_id));
            for g in gone {
                self.by_branch.remove(&g);
            }
        }
    }
}

/// Where a watch's branch is now.
pub(crate) enum Place {
    /// open, in this dataset under this branch name
    Found(Arc<crate::state::Dataset>, String),
    /// not known now: a dataset that could hold the branch is opening, failed to open,
    /// or is being deleted or renamed
    Unknown,
    /// no dataset of the catalog holds the branch
    Gone,
}

/// Find branch `bid`, last seen in dataset `dataset`. A dataset keeps its branches'
/// ids through a rename, so every dataset of the catalog is searched; the branch is
/// gone only when every one of them is open and none holds it, so a deletion or a
/// rename that is still draining, and may yet fail, removes no watch.
pub(crate) fn locate(st: &AppState, dataset: &str, bid: uuid::Uuid) -> Place {
    let mut unsure = st.any_leaving();
    let mut names: Vec<String> = st.catalog.list().into_iter().map(|i| i.name).collect();
    // the last known dataset first, which holds the branch unless it was renamed
    names.sort_by_key(|n| n != dataset);
    for name in names {
        let Some(d) = st.get(&name) else {
            unsure = true;
            continue;
        };
        match d.store.listed_branch_name(bid, &Default::default()) {
            Ok(Some(b)) => return Place::Found(d, b),
            Ok(None) => {}
            Err(_) => unsure = true,
        }
    }
    if unsure { Place::Unknown } else { Place::Gone }
}

/// What one run of a follower found.
pub(crate) struct Out {
    pub cursor: u64,
    pub found: Vec<(String, Match)>,
    pub truncated: BTreeMap<String, bool>,
}

/// One watch as the follower reads it.
struct W {
    id: String,
    owner: Owner,
    graph: Option<String>,
    scope: Scope,
    depth: Option<u32>,
}

/// The follower's own numbers for IRIs, which do not change with the store's
/// generations.
#[derive(Default)]
struct Names {
    iris: Vec<Arc<str>>,
    index: HashMap<Arc<str>, u32>,
}

impl Names {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&n) = self.index.get(s) {
            return n;
        }
        let n = self.iris.len() as u32;
        let a: Arc<str> = s.into();
        self.iris.push(a.clone());
        self.index.insert(a, n);
        n
    }

    fn get(&self, s: &str) -> Option<u32> {
        self.index.get(s).copied()
    }

    fn iri(&self, n: u32) -> &str {
        &self.iris[n as usize]
    }
}

/// The asserted hierarchy of one watched graph, in names.
#[derive(Default)]
struct Hierarchy {
    /// child → parent → how many hierarchy triples state the edge
    parents: HashMap<u32, BTreeMap<u32, u32>>,
    edges: usize,
    /// more than [`MAX_GRAPH_EDGES`] edges: none are kept and subtrees are not followed
    over: bool,
}

impl Hierarchy {
    fn add(&mut self, child: u32, parent: u32) {
        if self.over {
            return;
        }
        let n = self
            .parents
            .entry(child)
            .or_default()
            .entry(parent)
            .or_default();
        if *n == 0 {
            self.edges += 1;
        }
        *n += 1;
        if self.edges > MAX_GRAPH_EDGES {
            self.parents.clear();
            self.edges = 0;
            self.over = true;
        }
    }

    fn remove(&mut self, child: u32, parent: u32) {
        let Some(ps) = self.parents.get_mut(&child) else {
            return;
        };
        if let Some(n) = ps.get_mut(&parent) {
            *n -= 1;
            if *n == 0 {
                ps.remove(&parent);
                self.edges -= 1;
            }
        }
        if ps.is_empty() {
            self.parents.remove(&child);
        }
    }
}

/// The store ids of the current generation.
#[derive(Default)]
struct Binding {
    uid: Option<u64>,
    /// graph id → the graph's key
    graphs: HashMap<Id, Option<String>>,
    /// hierarchy predicate id → whether its subject is the child
    predicates: HashMap<Id, bool>,
    /// store id → name, for the names that have one
    names: HashMap<Id, u32>,
    /// ids decoded once and found to be no name, at most [`UNKNOWN`]
    unknown: HashSet<Id>,
    /// a watched graph or a hierarchy predicate has no id yet
    missing: bool,
}

/// The most ids [`Binding::unknown`] keeps before it starts again.
const UNKNOWN: usize = 1 << 20;
/// The names a follower keeps beyond four times those it uses before it starts again.
const NAMES_FLOOR: usize = if cfg!(test) { 64 } else { 1 << 16 };

pub(crate) struct Follower {
    cursor: u64,
    version: u64,
    watches: Vec<W>,
    names: Names,
    /// per graph: entity name → the watches whose entity it is
    entities: HashMap<Option<String>, HashMap<u32, Vec<usize>>>,
    /// graphs with a subtree watch
    graphs: BTreeMap<Option<String>, Hierarchy>,
    /// the graphs whose hierarchy must be read at the cursor
    load: BTreeSet<Option<String>>,
    binding: Binding,
    /// the store the subscription is on, by address, and the subscription
    sub: Option<(usize, DeltaSubscription)>,
}

impl Follower {
    pub(crate) fn new(cursor: u64) -> Follower {
        Follower {
            cursor,
            version: u64::MAX,
            watches: Vec::new(),
            names: Names::default(),
            entities: HashMap::new(),
            graphs: BTreeMap::new(),
            load: BTreeSet::new(),
            binding: Binding::default(),
            sub: None,
        }
    }

    /// Whether the follower keeps many more names than its hierarchies and watches use,
    /// which hierarchy nodes and watches that came and went leave behind.
    fn bloated(&self) -> bool {
        let used: usize = self
            .graphs
            .values()
            .map(|h| h.parents.len() + h.edges)
            .sum::<usize>()
            + self.watches.len();
        self.names.iris.len() > NAMES_FLOOR.max(4 * used)
    }

    /// The names the follower keeps (tests).
    #[cfg(test)]
    pub(crate) fn names(&self) -> usize {
        self.names.iris.len()
    }

    /// Read again from commit `cursor`, with the hierarchy read at it.
    fn rewind(&mut self, cursor: u64) {
        if cursor < self.cursor {
            self.cursor = cursor;
            self.load.extend(self.graphs.keys().cloned());
        }
    }

    /// Take the branch's current watches. `cursor` is the saved cursor, which a new
    /// watch can hold below the follower's own: the follower then reads from it again.
    pub(crate) fn sync(&mut self, ws: Vec<Watch>, version: u64, cursor: u64) {
        self.version = version;
        self.rewind(cursor);
        self.entities.clear();
        let mut watches = Vec::with_capacity(ws.len());
        for (i, w) in ws.into_iter().enumerate() {
            let entity = self.names.intern(&w.entity);
            self.entities
                .entry(w.graph.clone())
                .or_default()
                .entry(entity)
                .or_default()
                .push(i);
            watches.push(W {
                id: w.id,
                owner: w.owner,
                graph: w.graph,
                scope: w.scope,
                depth: w.depth,
            });
        }
        self.watches = watches;
        let wanted: BTreeSet<Option<String>> = self
            .watches
            .iter()
            .filter(|w| w.scope == Scope::Subtree)
            .map(|w| w.graph.clone())
            .collect();
        self.graphs.retain(|g, _| wanted.contains(g));
        for g in wanted {
            if !self.graphs.contains_key(&g) {
                self.load.insert(g);
            }
        }
        self.binding.uid = None;
    }

    /// Read the hierarchy of the graphs in `load`, or of every graph with `all`, from
    /// `snap`.
    fn load_graphs(&mut self, snap: &Snapshot, all: bool) {
        let keys: Vec<Option<String>> = if all {
            self.graphs
                .keys()
                .cloned()
                .chain(self.load.iter().cloned())
                .collect()
        } else {
            self.load.iter().cloned().collect()
        };
        for g in keys {
            let h = read_hierarchy(snap, g.as_deref(), &mut self.names);
            self.graphs.insert(g, h);
        }
        self.load.clear();
        self.binding.uid = None;
    }

    /// Bind the names to the ids of `snap`'s generation.
    fn bind(&mut self, snap: &Snapshot) {
        let uid = snap.generation.uid;
        if self.binding.uid == Some(uid) {
            if self.binding.missing {
                self.bind_missing(snap);
            }
            return;
        }
        let mut b = Binding {
            uid: Some(uid),
            ..Default::default()
        };
        let mut keys: BTreeSet<Option<String>> =
            self.watches.iter().map(|w| w.graph.clone()).collect();
        keys.extend(self.graphs.keys().cloned());
        for g in keys {
            let id = match &g {
                None => Some(Id::DEFAULT_GRAPH),
                Some(iri) => snap.lookup_iri(iri),
            };
            if let Some(id) = id {
                b.graphs.insert(id, g);
            }
        }
        for (p, child_is_subject) in HIERARCHY {
            if let Some(id) = snap.lookup_iri(p) {
                b.predicates.insert(id, child_is_subject);
            }
        }
        for (n, iri) in self.names.iris.iter().enumerate() {
            if let Some(id) = snap.lookup_iri(iri) {
                b.names.insert(id, n as u32);
            }
        }
        self.binding = b;
        self.binding.missing =
            self.missing_keys().next().is_some() || self.binding.predicates.len() < HIERARCHY.len();
    }

    /// The graph keys without an id.
    fn missing_keys(&self) -> impl Iterator<Item = Option<String>> + '_ {
        let bound: BTreeSet<&Option<String>> = self.binding.graphs.values().collect();
        self.watches
            .iter()
            .map(|w| w.graph.clone())
            .chain(self.graphs.keys().cloned())
            .filter(move |g| !bound.contains(g))
    }

    /// Look up the graphs and hierarchy predicates that had no id, which a commit in
    /// the same generation can add.
    fn bind_missing(&mut self, snap: &Snapshot) {
        let keys: BTreeSet<Option<String>> = self.missing_keys().collect();
        for g in keys {
            let id = match &g {
                None => Some(Id::DEFAULT_GRAPH),
                Some(iri) => snap.lookup_iri(iri),
            };
            if let Some(id) = id {
                self.binding.unknown.remove(&id);
                self.binding.graphs.insert(id, g);
            }
        }
        for (p, child_is_subject) in HIERARCHY {
            if let Some(id) = snap.lookup_iri(p) {
                self.binding.predicates.insert(id, child_is_subject);
            }
        }
        self.binding.missing =
            self.missing_keys().next().is_some() || self.binding.predicates.len() < HIERARCHY.len();
    }

    /// The name of the IRI with store id `id`, if it is one.
    fn name_of(&mut self, snap: &Snapshot, id: Id) -> Option<u32> {
        if let Some(&n) = self.binding.names.get(&id) {
            return Some(n);
        }
        if self.binding.unknown.contains(&id) {
            return None;
        }
        let n = match snap.term(id) {
            Some(oxrdf::Term::NamedNode(iri)) => self.names.get(iri.as_str()),
            _ => None,
        };
        match n {
            Some(n) => {
                self.binding.names.insert(id, n);
            }
            None => {
                if self.binding.unknown.len() >= UNKNOWN {
                    self.binding.unknown.clear();
                }
                self.binding.unknown.insert(id);
            }
        }
        n
    }

    /// Name the IRI with store id `id`, interning it.
    fn intern_id(&mut self, snap: &Snapshot, id: Id) -> Option<u32> {
        if let Some(&n) = self.binding.names.get(&id) {
            return Some(n);
        }
        let Some(oxrdf::Term::NamedNode(iri)) = snap.term(id) else {
            return None;
        };
        let n = self.names.intern(iri.as_str());
        self.binding.unknown.remove(&id);
        self.binding.names.insert(id, n);
        Some(n)
    }

    /// Read the branch's new commits.
    pub(crate) fn run(
        &mut self,
        st: &AppState,
        store: &Store,
        dataset: &str,
        branch: &str,
        bounds: &WatchSettings,
    ) -> Out {
        let fresh = self
            .sub
            .as_ref()
            .is_none_or(|(s, _)| *s != std::ptr::from_ref(store) as usize);
        if fresh {
            self.sub = Some((std::ptr::from_ref(store) as usize, store.subscribe_deltas()));
            if !self.graphs.is_empty() {
                self.load.extend(self.graphs.keys().cloned());
            }
        }
        let mut out = Out {
            cursor: self.cursor,
            found: Vec::new(),
            truncated: BTreeMap::new(),
        };
        let live = store.snapshot();
        if !self.load.is_empty() {
            if live.commit == self.cursor {
                self.load_graphs(&live, false);
            } else {
                match store.snapshot_at(&At::Commit(self.cursor), &HistoryOptions::default()) {
                    Ok((snap, _)) => self.load_graphs(&snap, false),
                    Err(e) => {
                        // the hierarchy can be read only at the head: the commits up
                        // to it are unverified, and matching goes on from there
                        tracing::warn!(target: "sparkles::watches", "the state at commit {} of {dataset}@{branch} is not readable, so its watches get an unverified match up to the head: {e}", self.cursor);
                        let mut access = Access::new(st, dataset, branch);
                        self.unverified(store, live.commit, &mut access, &mut out);
                        self.load_graphs(&live, true);
                        self.cursor = live.commit;
                    }
                }
            }
        }
        let mut access = Access::new(st, dataset, branch);
        'pages: for _ in 0..PAGES {
            if self.cursor >= store.snapshot().commit {
                break;
            }
            let o = ChangesOptions {
                max_commits: PAGE,
                ..Default::default()
            };
            let page = match store.commit_deltas(self.cursor, &o) {
                Ok(p) => p,
                Err(e) => {
                    // the changes are gone: every watch may have changed
                    let head = store.snapshot();
                    tracing::warn!(target: "sparkles::watches", "the changes after commit {} of {dataset}@{branch} are not readable, so its watches get an unverified match: {e}", self.cursor);
                    self.unverified(store, head.commit, &mut access, &mut out);
                    self.load_graphs(&head, true);
                    self.cursor = head.commit;
                    break;
                }
            };
            if page.commits.is_empty() {
                break;
            }
            self.bind(&page.snapshot);
            for c in &page.commits {
                if !c.complete {
                    // the changes of `c` are unknown, and the hierarchy can be read
                    // again only at the page's head: everything up to it is unverified
                    let at = page.snapshot.commit.max(c.seq);
                    self.unverified(store, c.seq, &mut access, &mut out);
                    if at > c.seq {
                        self.unverified(store, at, &mut access, &mut out);
                    }
                    self.load_graphs(&page.snapshot, true);
                    self.bind(&page.snapshot);
                    self.cursor = at;
                    continue 'pages;
                }
                self.commit(store, &page.snapshot, c, bounds, &mut access, &mut out);
                self.cursor = c.seq;
            }
        }
        for w in &self.watches {
            let over =
                w.scope == Scope::Subtree && self.graphs.get(&w.graph).is_some_and(|h| h.over);
            out.truncated.insert(w.id.clone(), over);
        }
        out.cursor = self.cursor;
        out
    }

    /// Commit `seq` may have changed every watch: record an unverified match.
    fn unverified(&self, store: &Store, seq: u64, access: &mut Access, out: &mut Out) {
        let (time, author) = meta(store, seq);
        for w in &self.watches {
            if !access.readable(&w.owner, w.graph.as_deref()) {
                continue;
            }
            out.found.push((
                w.id.clone(),
                Match {
                    commit: seq,
                    time: time.clone(),
                    own: author.as_deref() == Some(w.owner.key().as_str()),
                    author: author.clone(),
                    members: Vec::new(),
                    count: 0,
                    summary: false,
                    partial: false,
                    exact: false,
                },
            ));
        }
    }

    /// Match one commit whose changes are listed.
    fn commit(
        &mut self,
        store: &Store,
        snap: &Snapshot,
        c: &CommitDelta,
        bounds: &WatchSettings,
        access: &mut Access,
        out: &mut Out,
    ) {
        let changes: Vec<([Id; 4], bool)> = c
            .removed
            .iter()
            .map(|q| (*q, false))
            .chain(c.added.iter().map(|q| (*q, true)))
            .filter(|(q, _)| self.binding.graphs.contains_key(&q[3]))
            .collect();
        if changes.is_empty() {
            return;
        }
        // the hierarchy changes, whose IRIs are named first so that a new child counts
        let mut edges: Vec<(Option<String>, u32, u32, bool)> = Vec::new();
        for (q, add) in &changes {
            let g = &self.binding.graphs[&q[3]];
            if !self.graphs.contains_key(g) {
                continue;
            }
            let Some(&child_is_subject) = self.binding.predicates.get(&q[1]) else {
                continue;
            };
            let (c, p) = if child_is_subject {
                (q[0], q[2])
            } else {
                (q[2], q[0])
            };
            let g = g.clone();
            if let (Some(c), Some(p)) = (self.intern_id(snap, c), self.intern_id(snap, p)) {
                edges.push((g, c, p, *add));
            }
        }
        // the names each watched graph's changes mention
        let mut named: HashMap<Option<String>, BTreeSet<u32>> = HashMap::new();
        for (q, _) in &changes {
            let g = self.binding.graphs[&q[3]].clone();
            for &t in &q[..3] {
                if snap.term_kind(t) == TermKind::Iri
                    && let Some(n) = self.name_of(snap, t)
                {
                    named.entry(g.clone()).or_default().insert(n);
                }
            }
        }
        if named.is_empty() {
            return;
        }
        // watch → the names that matched it, and whether a walk stopped at a bound
        let mut hits: HashMap<usize, (BTreeSet<u32>, bool)> = HashMap::new();
        self.probe(&named, bounds, &mut hits);
        if !edges.is_empty() {
            for (g, c, p, add) in &edges {
                if let Some(h) = self.graphs.get_mut(g) {
                    if *add {
                        h.add(*c, *p);
                    } else {
                        h.remove(*c, *p);
                    }
                }
            }
            self.probe(&named, bounds, &mut hits);
        }
        if hits.is_empty() {
            return;
        }
        let (time, author) = meta(store, c.seq);
        let exact = true;
        let mut hits: Vec<_> = hits.into_iter().collect();
        hits.sort_by_key(|(i, _)| *i);
        for (i, (ns, partial)) in hits {
            let w = &self.watches[i];
            if !access.readable(&w.owner, w.graph.as_deref()) {
                continue;
            }
            let count = ns.len();
            let summary = count > bounds.large_commit as usize;
            let mut members: Vec<String> = if summary {
                Vec::new()
            } else {
                ns.iter().map(|&n| self.names.iri(n).to_string()).collect()
            };
            members.sort();
            members.truncate(MATCH_MEMBERS);
            out.found.push((
                w.id.clone(),
                Match {
                    commit: c.seq,
                    time: time.clone(),
                    own: author.as_deref() == Some(w.owner.key().as_str()),
                    author: author.clone(),
                    members,
                    count,
                    summary,
                    partial,
                    exact,
                },
            ));
        }
    }

    /// Match the names `named` in each graph against the watches, in the hierarchy as
    /// it is now.
    fn probe(
        &self,
        named: &HashMap<Option<String>, BTreeSet<u32>>,
        bounds: &WatchSettings,
        hits: &mut HashMap<usize, (BTreeSet<u32>, bool)>,
    ) {
        for (g, ns) in named {
            let Some(entities) = self.entities.get(g) else {
                continue;
            };
            let h = self.graphs.get(g).filter(|h| !h.over);
            let subtrees: Vec<usize> = entities
                .values()
                .flatten()
                .copied()
                .filter(|&i| self.watches[i].scope == Scope::Subtree)
                .collect();
            for &n in ns {
                // distance 0: the entity itself, for every scope
                if let Some(ws) = entities.get(&n) {
                    for &i in ws {
                        hits.entry(i).or_default().0.insert(n);
                    }
                }
                let Some(h) = h else { continue };
                if subtrees.is_empty() {
                    continue;
                }
                let stopped = walk(n, h, bounds.max_ancestors as usize, |anc, depth| {
                    if let Some(ws) = entities.get(&anc) {
                        for &i in ws {
                            let w = &self.watches[i];
                            if w.scope == Scope::Subtree && w.depth.is_none_or(|d| depth <= d) {
                                hits.entry(i).or_default().0.insert(n);
                            }
                        }
                    }
                });
                if stopped {
                    // the walk did not see every ancestor: any subtree watch here may
                    // have missed this IRI
                    for &i in &subtrees {
                        if let Some(e) = hits.get_mut(&i) {
                            e.1 = true;
                        }
                    }
                }
            }
        }
    }
}

/// Visit the ancestors of `start` breadth first with their distance, at most `max` of
/// them and `8 × max` edges; a cycle is visited once. `true` when a bound stopped it.
fn walk(start: u32, h: &Hierarchy, max: usize, mut f: impl FnMut(u32, u32)) -> bool {
    let mut seen: HashSet<u32> = HashSet::new();
    seen.insert(start);
    let mut queue: VecDeque<(u32, u32)> = VecDeque::new();
    queue.push_back((start, 0));
    let (mut visited, mut examined) = (0usize, 0usize);
    let max_edges = max.saturating_mul(8);
    while let Some((n, d)) = queue.pop_front() {
        let Some(ps) = h.parents.get(&n) else {
            continue;
        };
        for &p in ps.keys() {
            examined += 1;
            if examined > max_edges {
                return true;
            }
            if !seen.insert(p) {
                continue;
            }
            visited += 1;
            if visited > max {
                return true;
            }
            f(p, d + 1);
            queue.push_back((p, d + 1));
        }
    }
    false
}

/// The hierarchy of graph `g` (the default graph for `None`) in `snap`.
fn read_hierarchy(snap: &Snapshot, g: Option<&str>, names: &mut Names) -> Hierarchy {
    let mut h = Hierarchy::default();
    let gid = match g {
        None => Some(Id::DEFAULT_GRAPH),
        Some(iri) => snap.lookup_iri(iri),
    };
    let Some(gid) = gid else { return h };
    for (p, child_is_subject) in HIERARCHY {
        let Some(pid) = snap.lookup_iri(p) else {
            continue;
        };
        let mut each = |q: [Id; 4]| {
            if q[3] != gid || h.over {
                return;
            }
            let (Some(oxrdf::Term::NamedNode(s)), Some(oxrdf::Term::NamedNode(o))) =
                (snap.term(q[0]), snap.term(q[2]))
            else {
                return;
            };
            let (c, pa) = if child_is_subject { (s, o) } else { (o, s) };
            let (c, pa) = (names.intern(c.as_str()), names.intern(pa.as_str()));
            h.add(c, pa);
        };
        let r = snap.scan(Perm::Pos, &[pid.0], |c| {
            match c {
                Chunk::Block(b, s, e) => {
                    for i in s..e {
                        each(Perm::Pos.to_quad(&b.key(i)));
                    }
                }
                Chunk::Row(k) => each(Perm::Pos.to_quad(&k)),
            }
            Ok(true)
        });
        if let Err(e) = r {
            tracing::warn!(target: "sparkles::watches", "cannot read the hierarchy of a watched graph: {e}");
        }
    }
    h
}

/// A commit's time and author from the change log.
fn meta(store: &Store, seq: u64) -> (String, Option<String>) {
    match store.commit_record(seq) {
        Some(c) => (
            chrono::DateTime::from_timestamp_millis(c.timestamp_ms)
                .map(now_text)
                .unwrap_or_else(|| now_text(chrono::Utc::now())),
            c.author.map(|a| a.to_string()),
        ),
        None => (now_text(chrono::Utc::now()), None),
    }
}

/// Whether owners may read a watched graph now, decided once per owner and graph in a
/// run.
pub(crate) struct Access<'a> {
    st: &'a AppState,
    dataset: &'a str,
    branch: &'a str,
    cache: HashMap<(String, Option<String>), bool>,
}

impl<'a> Access<'a> {
    pub(crate) fn new(st: &'a AppState, dataset: &'a str, branch: &'a str) -> Self {
        Access {
            st,
            dataset,
            branch,
            cache: HashMap::new(),
        }
    }

    fn readable(&mut self, owner: &Owner, graph: Option<&str>) -> bool {
        let key = (owner.key(), graph.map(str::to_string));
        if let Some(&r) = self.cache.get(&key) {
            return r;
        }
        let r = owner_reads(self.st, owner, self.dataset, self.branch, graph);
        self.cache.insert(key, r);
        r
    }
}

/// Whether `owner` may read `graph` of `dataset` on `branch` under the current policy,
/// through the history endpoint, without triple protections.
pub(crate) fn owner_reads(
    st: &AppState,
    owner: &Owner,
    dataset: &str,
    branch: &str,
    graph: Option<&str>,
) -> bool {
    match owner_principal(st, owner) {
        Some(p) => super::http::reads(&p, dataset, branch, graph),
        None => false,
    }
}

/// The principal `owner` acts as now, `None` when it no longer exists or is not
/// admitted, or when auth was turned on or off since the watch was made.
fn owner_principal(st: &AppState, owner: &Owner) -> Option<crate::auth::Principal> {
    let Some(_auth) = st.auth.as_ref() else {
        return (owner.kind == Kind::Local).then(crate::auth::Principal::local);
    };
    if owner.kind == Kind::Local {
        return None;
    }
    #[cfg(feature = "auth")]
    {
        use crate::auth::{Access, Principal, PrincipalInfo, Scheme};
        let who = owner.identity();
        let grants = _auth.policy().identity_grants(&who)?;
        Some(
            Principal::new(owner.kind, &owner.name, Scheme::Session, Access::of(grants)).with_info(
                PrincipalInfo {
                    owner: Some(who),
                    ..Default::default()
                },
            ),
        )
    }
    #[cfg(not(feature = "auth"))]
    None
}
