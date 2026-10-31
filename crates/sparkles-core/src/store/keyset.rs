//! [`KeySet`], the sorted set of keys that holds one permutation of a [`Delta`].
//!
//! It is a persistent B+ tree: a clone shares every node, and a write copies only the
//! nodes on the path it changes, so a snapshot keeps its delta while the writer goes on.
//! Each branch keeps the number of keys under each of its children, which counts the
//! keys in a range with two descents. The query planner counts the rows of every scan it
//! considers, and the persistent ordered set this replaces could only count by walking
//! the range, so a planner over a delta of 100,000 quads spent most of a small query's
//! time stepping through the delta's keys. Leaves hold their keys in one sorted array,
//! which also makes iteration a walk over contiguous memory.
//!
//! [`Delta`]: super::Delta

use crate::index::{Key, pad};
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;

/// Most keys in a leaf (32 bytes each).
const LEAF_CAP: usize = 64;
/// Most children of a branch.
const BRANCH_CAP: usize = 32;

enum Node {
    Leaf(Vec<Key>),
    Branch(Branch),
}

struct Branch {
    children: Vec<Arc<Node>>,
    /// the largest key under each child
    lasts: Vec<Key>,
    /// the number of keys under each child
    counts: Vec<usize>,
}

/// A copy of `v` with room for one entry more than a node of capacity `cap` holds, so
/// that the insert that overflows it does not grow the allocation (a vector that grows
/// doubles, and a leaf would keep that room after its split).
fn with_room<T: Clone>(v: &[T], cap: usize) -> Vec<T> {
    let mut out = Vec::with_capacity(cap + 1);
    out.extend_from_slice(v);
    out
}

/// The entries of `v` from `at` on, moved to a vector with room as [`with_room`] gives.
fn split_with_room<T>(v: &mut Vec<T>, at: usize, cap: usize) -> Vec<T> {
    let mut right = Vec::with_capacity(cap + 1);
    right.extend(v.drain(at..));
    v.shrink_to(cap + 1);
    right
}

/// Counts the bytes a write allocates for nodes. `()` counts nothing and compiles away,
/// so the ordinary write paths are unchanged.
pub(crate) trait Meter {
    /// Whether the meter counts at all. The checks it needs are skipped when not.
    const COUNTS: bool;
    fn add(&mut self, bytes: u64);
}

impl Meter for () {
    const COUNTS: bool = false;
    #[inline(always)]
    fn add(&mut self, _: u64) {}
}

impl Meter for u64 {
    const COUNTS: bool = true;
    #[inline]
    fn add(&mut self, bytes: u64) {
        *self = self.saturating_add(bytes);
    }
}

/// `Arc::make_mut`, counting the copy it makes of a shared node. Nodes are never
/// held weakly, so a node is copied exactly when another set holds it too.
#[inline]
fn make_mut<'a, M: Meter>(node: &'a mut Arc<Node>, meter: &mut M) -> &'a mut Node {
    if M::COUNTS && Arc::strong_count(node) != 1 {
        meter.add(copy_bytes(node));
    }
    Arc::make_mut(node)
}

/// The allocations of a copy of `node` for a write (see [`Node::clone`]).
fn copy_bytes(node: &Node) -> u64 {
    let vectors = match node {
        Node::Leaf(_) => (LEAF_CAP + 1) * std::mem::size_of::<Key>(),
        Node::Branch(_) => {
            (BRANCH_CAP + 1)
                * (std::mem::size_of::<Arc<Node>>()
                    + std::mem::size_of::<Key>()
                    + std::mem::size_of::<usize>())
        }
    };
    (NODE_HEADER + vectors) as u64
}

/// The allocations of a node that is not yet shared.
fn owned_bytes(node: &Node) -> u64 {
    let vectors = match node {
        Node::Leaf(v) => v.capacity() * std::mem::size_of::<Key>(),
        Node::Branch(b) => {
            b.children.capacity() * std::mem::size_of::<Arc<Node>>()
                + b.lasts.capacity() * std::mem::size_of::<Key>()
                + b.counts.capacity() * std::mem::size_of::<usize>()
        }
    };
    (NODE_HEADER + vectors) as u64
}

/// The shared allocation that holds a node, with its reference counts.
const NODE_HEADER: usize = 2 * std::mem::size_of::<usize>() + std::mem::size_of::<Node>();

impl Clone for Node {
    /// A copy of a node for a write, with room for the write's insert.
    fn clone(&self) -> Node {
        match self {
            Node::Leaf(v) => Node::Leaf(with_room(v, LEAF_CAP)),
            Node::Branch(b) => Node::Branch(Branch {
                children: with_room(&b.children, BRANCH_CAP),
                lasts: with_room(&b.lasts, BRANCH_CAP),
                counts: with_room(&b.counts, BRANCH_CAP),
            }),
        }
    }
}

impl Branch {
    /// The right half of a branch that holds more than its capacity.
    fn split(&mut self) -> Branch {
        let at = self.children.len() / 2;
        Branch {
            children: split_with_room(&mut self.children, at, BRANCH_CAP),
            lasts: split_with_room(&mut self.lasts, at, BRANCH_CAP),
            counts: split_with_room(&mut self.counts, at, BRANCH_CAP),
        }
    }

    /// The child whose range holds `k`: the first whose largest key is not less than
    /// `k`, or the last child.
    #[inline]
    fn route(&self, k: &Key) -> usize {
        self.lasts
            .partition_point(|l| l < k)
            .min(self.children.len() - 1)
    }

    fn total(&self) -> usize {
        self.counts.iter().sum()
    }
}

impl Node {
    fn last(&self) -> Key {
        match self {
            Node::Leaf(v) => *v.last().expect("a leaf in a tree is not empty"),
            Node::Branch(b) => *b.lasts.last().expect("a branch has children"),
        }
    }

    fn total(&self) -> usize {
        match self {
            Node::Leaf(v) => v.len(),
            Node::Branch(b) => b.total(),
        }
    }

    /// Entries: keys of a leaf, children of a branch.
    fn width(&self) -> usize {
        match self {
            Node::Leaf(v) => v.len(),
            Node::Branch(b) => b.children.len(),
        }
    }

    fn contains(&self, k: &Key) -> bool {
        let mut n = self;
        loop {
            match n {
                Node::Leaf(v) => return v.binary_search(k).is_ok(),
                Node::Branch(b) => n = &b.children[b.route(k)],
            }
        }
    }

    /// The number of keys less than `k`, or not greater than `k` when `inclusive`.
    fn rank(&self, k: &Key, inclusive: bool) -> usize {
        let mut n = self;
        let mut r = 0;
        loop {
            match n {
                Node::Leaf(v) => {
                    return r + if inclusive {
                        v.partition_point(|x| x <= k)
                    } else {
                        v.partition_point(|x| x < k)
                    };
                }
                Node::Branch(b) => {
                    let i = if inclusive {
                        b.lasts.partition_point(|l| l <= k)
                    } else {
                        b.lasts.partition_point(|l| l < k)
                    };
                    r += b.counts[..i].iter().sum::<usize>();
                    if i == b.children.len() {
                        return r;
                    }
                    n = &b.children[i];
                }
            }
        }
    }

    /// Insert `k`: `None` when it is already in the tree, else the right half of the
    /// node when it splits. `shares` is set to whether another key of the tree has the
    /// first `n` columns of `k`, when the keys next to it in its leaf tell (it is left
    /// `None` when `k` lands at an end of its leaf and the other neighbour differs).
    fn insert<M: Meter>(
        &mut self,
        k: Key,
        n: usize,
        shares: &mut Option<bool>,
        meter: &mut M,
    ) -> Option<Option<Node>> {
        match self {
            Node::Leaf(v) => {
                let at = v.binary_search(&k).err()?;
                v.insert(at, k);
                let same = |x: &Key| x[..n] == k[..n];
                let before = at.checked_sub(1).map(|i| &v[i]);
                let after = v.get(at + 1);
                *shares = if before.is_some_and(same) || after.is_some_and(same) {
                    Some(true)
                } else if before.is_some() && after.is_some() {
                    // the keys with a prefix are consecutive
                    Some(false)
                } else {
                    None
                };
                Some((v.len() > LEAF_CAP).then(|| {
                    let at = v.len() / 2;
                    Node::Leaf(split_with_room(v, at, LEAF_CAP))
                }))
            }
            Node::Branch(b) => {
                let i = b.route(&k);
                let split = make_mut(&mut b.children[i], meter).insert(k, n, shares, meter)?;
                match split {
                    None => {
                        b.counts[i] += 1;
                        if k > b.lasts[i] {
                            b.lasts[i] = k;
                        }
                    }
                    Some(right) => {
                        let left = &b.children[i];
                        b.counts[i] = left.total();
                        b.lasts[i] = left.last();
                        b.counts.insert(i + 1, right.total());
                        b.lasts.insert(i + 1, right.last());
                        meter.add(owned_bytes(&right));
                        b.children.insert(i + 1, Arc::new(right));
                    }
                }
                Some((b.children.len() > BRANCH_CAP).then(|| Node::Branch(b.split())))
            }
        }
    }

    /// Remove `k`, which is in the tree. A child left with fewer than a quarter of its
    /// capacity is merged with a neighbour, or the two share their entries evenly when
    /// together they are more than one node holds.
    fn remove<M: Meter>(&mut self, k: &Key, meter: &mut M) {
        match self {
            Node::Leaf(v) => {
                let at = v.binary_search(k).expect("the key is in the tree");
                v.remove(at);
            }
            Node::Branch(b) => {
                let i = b.route(k);
                let child = make_mut(&mut b.children[i], meter);
                child.remove(k, meter);
                b.counts[i] -= 1;
                let width = child.width();
                if width == 0 {
                    b.children.remove(i);
                    b.lasts.remove(i);
                    b.counts.remove(i);
                    return;
                }
                b.lasts[i] = child.last();
                let cap = match child {
                    Node::Leaf(_) => LEAF_CAP,
                    Node::Branch(_) => BRANCH_CAP,
                };
                if width < cap / 4 && b.children.len() > 1 {
                    let j = if i + 1 < b.children.len() { i } else { i - 1 };
                    rebalance(b, j, cap, meter);
                }
            }
        }
    }
}

/// Merge children `j` and `j + 1` of `b`, or share their entries evenly when together
/// they are more than `cap`.
fn rebalance<M: Meter>(b: &mut Branch, j: usize, cap: usize, meter: &mut M) {
    let right = b.children.remove(j + 1);
    b.lasts.remove(j + 1);
    b.counts.remove(j + 1);
    if M::COUNTS && Arc::strong_count(&right) != 1 {
        meter.add(copy_bytes(&right));
    }
    let right = Arc::unwrap_or_clone(right);
    let left = make_mut(&mut b.children[j], meter);
    let before = if M::COUNTS { owned_bytes(left) } else { 0 };
    let split = match (left, right) {
        (Node::Leaf(l), Node::Leaf(r)) => {
            l.extend(r);
            let at = l.len() / 2;
            (l.len() > cap).then(|| Node::Leaf(split_with_room(l, at, cap)))
        }
        (Node::Branch(l), Node::Branch(r)) => {
            l.children.extend(r.children);
            l.lasts.extend(r.lasts);
            l.counts.extend(r.counts);
            (l.children.len() > cap).then(|| Node::Branch(l.split()))
        }
        _ => unreachable!("siblings are at the same level"),
    };
    // a vector that grows moves to a new allocation
    if M::COUNTS {
        let after = owned_bytes(&b.children[j]);
        if after > before {
            meter.add(after);
        }
    }
    let left = &b.children[j];
    b.counts[j] = left.total();
    b.lasts[j] = left.last();
    if let Some(r) = split {
        b.counts.insert(j + 1, r.total());
        b.lasts.insert(j + 1, r.last());
        meter.add(owned_bytes(&r));
        b.children.insert(j + 1, Arc::new(r));
    }
}

/// The most bytes one node allocates. A merge can extend a node's vectors to twice
/// the room a copy gives before it splits again.
const NODE_BOUND: u64 = {
    let header = NODE_HEADER;
    let leaf = 2 * (LEAF_CAP + 1) * std::mem::size_of::<Key>();
    let branch = 2
        * (BRANCH_CAP + 1)
        * (std::mem::size_of::<Arc<Node>>()
            + std::mem::size_of::<Key>()
            + std::mem::size_of::<usize>());
    (header + if leaf > branch { leaf } else { branch }) as u64
};

/// A persistent sorted set of keys with range counts (see the module docs).
#[derive(Clone, Default)]
pub struct KeySet {
    root: Option<Arc<Node>>,
    len: usize,
}

impl KeySet {
    pub fn new() -> KeySet {
        KeySet::default()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The levels from the root to a leaf (0 for an empty set).
    pub(crate) fn depth(&self) -> usize {
        let mut depth = 0;
        let mut node = self.root.as_deref();
        while let Some(n) = node {
            depth += 1;
            node = match n {
                Node::Leaf(_) => None,
                Node::Branch(b) => b.children.first().map(|c| &**c),
            };
        }
        depth
    }

    /// An upper bound on the bytes one insert or remove allocates in this set.
    pub(crate) fn mutation_bound(&self) -> u64 {
        let levels = self.depth() as u64 + 1;
        // per level: a path copy, a copied sibling, a merged node's growth and a split;
        // then a new root and its new child
        levels
            .saturating_mul(4)
            .saturating_add(2)
            .saturating_mul(NODE_BOUND)
    }

    /// The bound [`mutation_bound`](Self::mutation_bound) adds for each level.
    pub(crate) fn level_bound() -> u64 {
        4 * NODE_BOUND
    }

    /// The allocated bytes of the nodes this set alone owns: the nodes a write copied or
    /// created since the set was cloned. Nodes shared with another set are not counted,
    /// and neither is anything below them. `check` runs every 1024 nodes.
    #[cfg(test)]
    pub(crate) fn unshared_bytes(
        &self,
        mut check: impl FnMut() -> crate::error::Result<()>,
    ) -> crate::error::Result<(u64, u64)> {
        let Some(root) = &self.root else {
            return Ok((0, 0));
        };
        let mut stack: Vec<&Arc<Node>> = Vec::new();
        if Arc::strong_count(root) == 1 && Arc::weak_count(root) == 0 {
            stack.push(root);
        }
        let (mut bytes, mut nodes) = (0u64, 0u64);
        while let Some(node) = stack.pop() {
            nodes += 1;
            if nodes.is_multiple_of(1024) {
                check()?;
            }
            bytes = bytes.saturating_add(owned_bytes(node));
            if let Node::Branch(b) = &**node {
                for child in &b.children {
                    if Arc::strong_count(child) == 1 && Arc::weak_count(child) == 0 {
                        stack.push(child);
                    }
                }
            }
        }
        Ok((bytes, nodes))
    }

    /// Whether `other` is this set or an unchanged clone of it: both are empty, or both
    /// share one root and hold as many keys. A set changed after a clone has a copied
    /// root, so this is false for it even when it holds the same keys.
    pub fn same(&self, other: &KeySet) -> bool {
        self.len == other.len
            && match (&self.root, &other.root) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }

    pub fn contains(&self, k: &Key) -> bool {
        self.root.as_ref().is_some_and(|r| r.contains(k))
    }

    /// Insert `k`; true if it was not in the set. In one descent, which copies the nodes
    /// on its path that a clone shares even when the key is already there: the delta's
    /// writer inserts keys that are mostly new.
    pub fn insert(&mut self, k: Key) -> bool {
        self.insert_at(k, 0, &mut None, &mut ()).is_some()
    }

    /// Insert `k`: `None` if it was in the set, else whether another key of the set
    /// starts with the first `n` columns of `k`. That is mostly read off the keys next
    /// to `k` in its leaf, so it seldom costs more than the insert.
    pub fn insert_sharing(&mut self, k: Key, n: usize) -> Option<bool> {
        let mut shares = None;
        self.insert_at(k, n, &mut shares, &mut ())?;
        Some(shares.unwrap_or_else(|| {
            let prefix = &k[..n];
            self.count_between(&pad(prefix, 0), &pad(prefix, u64::MAX)) > 1
        }))
    }

    fn insert_at<M: Meter>(
        &mut self,
        k: Key,
        n: usize,
        shares: &mut Option<bool>,
        meter: &mut M,
    ) -> Option<()> {
        let Some(root) = self.root.as_mut() else {
            let leaf = Node::Leaf(with_room(&[k], LEAF_CAP));
            meter.add(owned_bytes(&leaf));
            self.root = Some(Arc::new(leaf));
            self.len = 1;
            *shares = Some(false);
            return Some(());
        };
        let split = make_mut(root, meter).insert(k, n, shares, meter)?;
        self.len += 1;
        if let Some(right) = split {
            let left = self.root.take().expect("the root is set");
            meter.add(owned_bytes(&right));
            // the root gets the room a copy has, so the inserts that fill it later
            // do not grow its vectors past what the meter counted here
            let mut lasts = Vec::with_capacity(BRANCH_CAP + 1);
            lasts.extend([left.last(), right.last()]);
            let mut counts = Vec::with_capacity(BRANCH_CAP + 1);
            counts.extend([left.total(), right.total()]);
            let mut children = Vec::with_capacity(BRANCH_CAP + 1);
            children.extend([left, Arc::new(right)]);
            let root = Node::Branch(Branch {
                lasts,
                counts,
                children,
            });
            meter.add(owned_bytes(&root));
            self.root = Some(Arc::new(root));
        }
        Some(())
    }

    /// [`insert`](Self::insert), adding to `meter` the bytes of every node it copies or
    /// creates.
    pub(crate) fn insert_metered(&mut self, k: Key, meter: &mut u64) -> bool {
        self.insert_at(k, 0, &mut None, meter).is_some()
    }

    /// [`remove`](Self::remove), adding to `meter` the bytes of every node it copies or
    /// grows.
    pub(crate) fn remove_metered(&mut self, k: &Key, meter: &mut u64) -> bool {
        self.remove_with(k, meter)
    }

    /// Remove `k`; true if it was in the set. An absent key copies no node.
    pub fn remove(&mut self, k: &Key) -> bool {
        self.remove_with(k, &mut ())
    }

    fn remove_with<M: Meter>(&mut self, k: &Key, meter: &mut M) -> bool {
        if !self.contains(k) {
            return false;
        }
        self.len -= 1;
        let root = self.root.as_mut().expect("a set holding a key has a root");
        make_mut(root, meter).remove(k, meter);
        // a branch left with one child gives way to it
        loop {
            match self.root.as_deref() {
                Some(Node::Leaf(v)) if v.is_empty() => self.root = None,
                Some(Node::Branch(b)) if b.children.len() == 1 => {
                    self.root = Some(b.children[0].clone());
                }
                _ => break,
            }
        }
        true
    }

    /// The number of keys in `[lo, hi]`, in time logarithmic in the size of the set.
    pub fn count_between(&self, lo: &Key, hi: &Key) -> usize {
        match &self.root {
            Some(r) if lo <= hi => r.rank(hi, true) - r.rank(lo, false),
            _ => 0,
        }
    }

    /// The number of keys in `range`.
    pub fn count_range(&self, range: impl RangeBounds<Key>) -> usize {
        let Some(r) = &self.root else { return 0 };
        let below = match range.start_bound() {
            Bound::Unbounded => 0,
            Bound::Included(k) => r.rank(k, false),
            Bound::Excluded(k) => r.rank(k, true),
        };
        let upto = match range.end_bound() {
            Bound::Unbounded => self.len,
            Bound::Included(k) => r.rank(k, true),
            Bound::Excluded(k) => r.rank(k, false),
        };
        upto.saturating_sub(below)
    }

    /// Whether a key lies in `range`: one descent, cheaper than starting an iterator.
    pub fn intersects(&self, range: impl RangeBounds<Key>) -> bool {
        let first = match range.start_bound() {
            Bound::Unbounded => self.first_where(|_| false),
            Bound::Included(k) => self.first_where(|x| x < k),
            Bound::Excluded(k) => self.first_where(|x| x <= k),
        };
        first.is_some_and(|f| match range.end_bound() {
            Bound::Unbounded => true,
            Bound::Included(h) => f <= h,
            Bound::Excluded(h) => f < h,
        })
    }

    /// The first key for which `before` is false (`before` holds for a prefix of the
    /// keys in order).
    #[inline]
    fn first_where(&self, before: impl Fn(&Key) -> bool) -> Option<&Key> {
        let mut n = self.root.as_deref()?;
        loop {
            match n {
                Node::Leaf(v) => return v.get(v.partition_point(&before)),
                Node::Branch(b) => {
                    let i = b.lasts.partition_point(&before);
                    n = b.children.get(i)?;
                }
            }
        }
    }

    /// The keys in order.
    pub fn iter(&self) -> Iter<'_> {
        self.range(..)
    }

    /// The keys in `range`, in order.
    pub fn range(&self, range: impl RangeBounds<Key>) -> Iter<'_> {
        let mut it = Iter {
            root: self.root.as_deref(),
            leaf: &[],
            pos: 0,
            hi: range.end_bound().cloned(),
        };
        let found = match (it.root, range.start_bound()) {
            (None, _) => None,
            (Some(r), Bound::Unbounded) => seek(r, |_| false),
            (Some(r), Bound::Included(k)) => seek(r, |x| x < k),
            (Some(r), Bound::Excluded(k)) => seek(r, |x| x <= k),
        };
        match found {
            Some((leaf, pos)) => (it.leaf, it.pos) = (leaf, pos),
            None => it.root = None,
        }
        it
    }

    pub fn first(&self) -> Option<&Key> {
        self.iter().next()
    }
}

impl std::fmt::Debug for KeySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

impl PartialEq for KeySet {
    fn eq(&self, other: &KeySet) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl Eq for KeySet {}

impl FromIterator<Key> for KeySet {
    fn from_iter<I: IntoIterator<Item = Key>>(iter: I) -> KeySet {
        let mut s = KeySet::new();
        s.extend(iter);
        s
    }
}

impl Extend<Key> for KeySet {
    fn extend<I: IntoIterator<Item = Key>>(&mut self, iter: I) {
        for k in iter {
            self.insert(k);
        }
    }
}

impl<'a> IntoIterator for &'a KeySet {
    type Item = &'a Key;
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Iter<'a> {
        self.iter()
    }
}

/// The leaf of the first key of the tree under `n` for which `before` is false, and the
/// key's position in it (`before` holds for a prefix of the keys in order). `None` when
/// it holds for every key.
fn seek(mut n: &Node, before: impl Fn(&Key) -> bool) -> Option<(&[Key], usize)> {
    loop {
        match n {
            Node::Leaf(v) => {
                let pos = v.partition_point(&before);
                return (pos < v.len()).then_some((&v[..], pos));
            }
            Node::Branch(b) => n = b.children.get(b.lasts.partition_point(&before))?,
        }
    }
}

/// The keys of a [`KeySet`] range, in order. It keeps no path: the next leaf is found by
/// a descent from the root past the last key of the current one, which costs a few
/// binary searches per leaf of up to 64 keys.
#[derive(Clone)]
pub struct Iter<'a> {
    /// the tree's root, `None` once the range is done
    root: Option<&'a Node>,
    leaf: &'a [Key],
    pos: usize,
    hi: Bound<Key>,
}

impl<'a> Iter<'a> {
    /// Move to the first key of the next leaf; false at the end of the tree.
    fn next_leaf(&mut self) -> bool {
        let next = match (self.root, self.leaf.last()) {
            (Some(r), Some(last)) => seek(r, |x| x <= last),
            _ => None,
        };
        match next {
            Some((leaf, pos)) => {
                (self.leaf, self.pos) = (leaf, pos);
                true
            }
            None => {
                self.end();
                false
            }
        }
    }

    fn end(&mut self) {
        self.root = None;
        self.leaf = &[];
        self.pos = 0;
    }

    #[inline]
    fn before_end(&self, k: &Key) -> bool {
        match &self.hi {
            Bound::Unbounded => true,
            Bound::Included(h) => k <= h,
            Bound::Excluded(h) => k < h,
        }
    }

    /// The keys left in the current leaf that are in the range, as one slice (empty at
    /// the end), consumed.
    pub fn next_run(&mut self) -> &'a [Key] {
        if self.pos >= self.leaf.len() && !self.next_leaf() {
            return &[];
        }
        let rest = &self.leaf[self.pos..];
        let n = match &self.hi {
            Bound::Unbounded => rest.len(),
            Bound::Included(h) => rest.partition_point(|x| x <= h),
            Bound::Excluded(h) => rest.partition_point(|x| x < h),
        };
        if n < rest.len() {
            // the range ends in this leaf
            self.end();
        } else {
            self.pos = self.leaf.len();
        }
        &rest[..n]
    }
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a Key;

    #[inline]
    fn next(&mut self) -> Option<&'a Key> {
        if self.pos >= self.leaf.len() && !self.next_leaf() {
            return None;
        }
        let k = &self.leaf[self.pos];
        if !self.before_end(k) {
            self.end();
            return None;
        }
        self.pos += 1;
        Some(k)
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn metered_writes_count_every_node_a_clone_comes_to_own() {
        let key = |i: u64| [i / 7, i % 7, i, 0];
        let base: KeySet = (0..20_000).map(key).collect();
        for step in [1u64, 3, 10, 97] {
            let mut set = base.clone();
            let mut inserted = 0u64;
            for i in (20_000..24_000).step_by(step as usize) {
                let bound = set.mutation_bound();
                let mut one = 0u64;
                assert!(set.insert_metered(key(i), &mut one));
                assert!(one <= bound);
                inserted += one;
            }
            let (owned, _) = set.unshared_bytes(|| Ok(())).unwrap();
            assert!(inserted >= owned, "inserts counted {inserted} of {owned}");
            let mut set = base.clone();
            let mut removed = 0u64;
            for i in (0..20_000).step_by(step as usize) {
                let bound = set.mutation_bound();
                let mut one = 0u64;
                assert!(set.remove_metered(&key(i), &mut one));
                assert!(one <= bound);
                removed += one;
            }
            let (owned, _) = set.unshared_bytes(|| Ok(())).unwrap();
            assert!(removed >= owned, "removals counted {removed} of {owned}");
            assert!(set.iter().all(|k| (k[2] % step) != 0));
        }
        // a clone that writes nothing owns nothing
        assert_eq!(base.clone().unshared_bytes(|| Ok(())).unwrap(), (0, 0));
    }

    #[test]
    fn metered_inserts_into_an_empty_set_count_every_node() {
        // New roots and their children are written by the metered path alone, so a
        // root that grows its vectors later must be counted too.
        for count in [100u64, 1_000, 5_000, 40_000] {
            let mut set = KeySet::new();
            let mut counted = 0u64;
            for i in 0..count {
                assert!(set.insert_metered([i, 0, 0, 0], &mut counted));
            }
            let (owned, _) = set.unshared_bytes(|| Ok(())).unwrap();
            assert!(
                counted >= owned,
                "{count} inserts counted {counted} of {owned}"
            );
        }
    }
    use super::*;
    use std::collections::BTreeSet;

    /// A small deterministic generator (xorshift) for the randomized checks.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn key(x: u64) -> Key {
        [x / 97, x % 97, x % 5, 0]
    }

    fn check(s: &KeySet, m: &BTreeSet<Key>, rng: &mut Rng, span: u64) {
        assert_eq!(s.len(), m.len());
        assert!(s.iter().eq(m.iter()));
        if let Some(r) = &s.root {
            assert_eq!(r.total(), m.len());
            sane(r, true);
        }
        for _ in 0..20 {
            let (a, b) = (key(rng.next() % span), key(rng.next() % span));
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            assert_eq!(s.count_between(&lo, &hi), m.range(lo..=hi).count());
            assert_eq!(s.count_range(lo..hi), m.range(lo..hi).count());
            assert_eq!(
                s.count_range((Bound::Excluded(lo), Bound::Unbounded)),
                m.range((Bound::Excluded(lo), Bound::Unbounded)).count()
            );
            assert!(s.range(lo..=hi).eq(m.range(lo..=hi)));
            assert_eq!(s.intersects(lo..=hi), m.range(lo..=hi).next().is_some());
            assert_eq!(s.intersects(..lo), m.range(..lo).next().is_some());
            assert_eq!(
                s.intersects((Bound::Excluded(lo), Bound::Unbounded)),
                m.range((Bound::Excluded(lo), Bound::Unbounded))
                    .next()
                    .is_some()
            );
            if lo < hi {
                assert!(
                    s.range((Bound::Excluded(lo), Bound::Excluded(hi)))
                        .eq(m.range((Bound::Excluded(lo), Bound::Excluded(hi))))
                );
            }
            let mut it = s.range(lo..=hi);
            let mut runs = Vec::new();
            loop {
                let r = it.next_run();
                if r.is_empty() {
                    break;
                }
                runs.extend_from_slice(r);
            }
            assert!(runs.iter().eq(m.range(lo..=hi)));
            assert_eq!(s.contains(&a), m.contains(&a));
        }
    }

    /// The structural invariants: counts and largest keys agree with the children, and
    /// every leaf of a tree is at the same depth. Returns the depth.
    fn sane(n: &Node, root: bool) -> usize {
        match n {
            Node::Leaf(v) => {
                assert!(v.windows(2).all(|w| w[0] < w[1]));
                assert!(v.len() <= LEAF_CAP && (root || !v.is_empty()));
                0
            }
            Node::Branch(b) => {
                assert!(b.children.len() <= BRANCH_CAP && b.children.len() > root as usize);
                let depths: Vec<usize> = b.children.iter().map(|c| sane(c, false)).collect();
                assert!(depths.windows(2).all(|w| w[0] == w[1]));
                for (i, c) in b.children.iter().enumerate() {
                    assert_eq!(b.counts[i], c.total());
                    assert_eq!(b.lasts[i], c.last());
                }
                assert!(b.lasts.windows(2).all(|w| w[0] < w[1]));
                depths[0] + 1
            }
        }
    }

    #[test]
    fn matches_an_ordered_set_under_random_writes() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for span in [50u64, 3_000, 40_000] {
            let (mut s, mut m) = (KeySet::new(), BTreeSet::new());
            let mut snaps: Vec<(KeySet, BTreeSet<Key>)> = Vec::new();
            for round in 0..30_000 {
                let k = key(rng.next() % span);
                // phases that grow the set, then shrink it to nothing and grow it again
                let grow = (round / 5_000) % 2 == 0;
                let insert = rng.next() % 10 < if grow { 8 } else { 2 };
                if insert && round % 3 == 0 {
                    // insert_sharing: whether another key has the first one or two columns
                    let n = 1 + round % 2;
                    let others = m
                        .range(pad(&k[..n], 0)..=pad(&k[..n], u64::MAX))
                        .any(|x| *x != k);
                    let want = (!m.contains(&k)).then_some(others);
                    assert_eq!(s.insert_sharing(k, n), want);
                    m.insert(k);
                } else if insert {
                    assert_eq!(s.insert(k), m.insert(k));
                } else {
                    assert_eq!(s.remove(&k), m.remove(&k));
                }
                if round % 2_000 == 0 {
                    check(&s, &m, &mut rng, span);
                    snaps.push((s.clone(), m.clone()));
                }
            }
            check(&s, &m, &mut rng, span);
            // the clones kept their contents while the set changed
            for (s, m) in &snaps {
                check(s, m, &mut rng, span);
            }
            // empty it in order, which leaves underfull nodes behind for the rebalance
            let all: Vec<Key> = m.iter().copied().collect();
            for (i, k) in all.iter().enumerate() {
                assert!(s.remove(k));
                m.remove(k);
                if i % 1_000 == 0 {
                    check(&s, &m, &mut rng, span);
                }
            }
            assert!(s.is_empty() && s.root.is_none());
        }
    }

    #[test]
    fn removing_an_absent_key_copies_nothing() {
        let s: KeySet = (0..10_000).map(key).collect();
        let mut t = s.clone();
        assert!(!t.remove(&key(20_000)));
        assert!(Arc::ptr_eq(
            s.root.as_ref().unwrap(),
            t.root.as_ref().unwrap()
        ));
    }
}
