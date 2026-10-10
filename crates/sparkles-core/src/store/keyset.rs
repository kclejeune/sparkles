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
/// Deeper than any tree of `u64::MAX` keys gets with these capacities.
const MAX_DEPTH: usize = 16;

#[derive(Clone)]
enum Node {
    Leaf(Vec<Key>),
    Branch(Branch),
}

#[derive(Clone)]
struct Branch {
    children: Vec<Arc<Node>>,
    /// the largest key under each child
    lasts: Vec<Key>,
    /// the number of keys under each child
    counts: Vec<usize>,
}

impl Branch {
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
    fn insert(&mut self, k: Key, n: usize, shares: &mut Option<bool>) -> Option<Option<Node>> {
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
                Some((v.len() > LEAF_CAP).then(|| Node::Leaf(v.split_off(v.len() / 2))))
            }
            Node::Branch(b) => {
                let i = b.route(&k);
                let split = Arc::make_mut(&mut b.children[i]).insert(k, n, shares)?;
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
                        b.children.insert(i + 1, Arc::new(right));
                    }
                }
                Some((b.children.len() > BRANCH_CAP).then(|| {
                    let at = b.children.len() / 2;
                    Node::Branch(Branch {
                        children: b.children.split_off(at),
                        lasts: b.lasts.split_off(at),
                        counts: b.counts.split_off(at),
                    })
                }))
            }
        }
    }

    /// Remove `k`, which is in the tree. A child left with fewer than a quarter of its
    /// capacity is merged with a neighbour, or the two share their entries evenly when
    /// together they are more than one node holds.
    fn remove(&mut self, k: &Key) {
        match self {
            Node::Leaf(v) => {
                let at = v.binary_search(k).expect("the key is in the tree");
                v.remove(at);
            }
            Node::Branch(b) => {
                let i = b.route(k);
                let child = Arc::make_mut(&mut b.children[i]);
                child.remove(k);
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
                    rebalance(b, j, cap);
                }
            }
        }
    }
}

/// Merge children `j` and `j + 1` of `b`, or share their entries evenly when together
/// they are more than `cap`.
fn rebalance(b: &mut Branch, j: usize, cap: usize) {
    let right = b.children.remove(j + 1);
    b.lasts.remove(j + 1);
    b.counts.remove(j + 1);
    let right = Arc::unwrap_or_clone(right);
    let left = Arc::make_mut(&mut b.children[j]);
    let split = match (left, right) {
        (Node::Leaf(l), Node::Leaf(r)) => {
            l.extend(r);
            (l.len() > cap).then(|| Node::Leaf(l.split_off(l.len() / 2)))
        }
        (Node::Branch(l), Node::Branch(r)) => {
            l.children.extend(r.children);
            l.lasts.extend(r.lasts);
            l.counts.extend(r.counts);
            (l.children.len() > cap).then(|| {
                let at = l.children.len() / 2;
                Node::Branch(Branch {
                    children: l.children.split_off(at),
                    lasts: l.lasts.split_off(at),
                    counts: l.counts.split_off(at),
                })
            })
        }
        _ => unreachable!("siblings are at the same level"),
    };
    let left = &b.children[j];
    b.counts[j] = left.total();
    b.lasts[j] = left.last();
    if let Some(r) = split {
        b.counts.insert(j + 1, r.total());
        b.lasts.insert(j + 1, r.last());
        b.children.insert(j + 1, Arc::new(r));
    }
}

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

    pub fn contains(&self, k: &Key) -> bool {
        self.root.as_ref().is_some_and(|r| r.contains(k))
    }

    /// Insert `k`; true if it was not in the set. In one descent, which copies the nodes
    /// on its path that a clone shares even when the key is already there: the delta's
    /// writer inserts keys that are mostly new.
    pub fn insert(&mut self, k: Key) -> bool {
        self.insert_at(k, 0, &mut None).is_some()
    }

    /// Insert `k`: `None` if it was in the set, else whether another key of the set
    /// starts with the first `n` columns of `k`. That is mostly read off the keys next
    /// to `k` in its leaf, so it seldom costs more than the insert.
    pub fn insert_sharing(&mut self, k: Key, n: usize) -> Option<bool> {
        let mut shares = None;
        self.insert_at(k, n, &mut shares)?;
        Some(shares.unwrap_or_else(|| {
            let prefix = &k[..n];
            self.count_between(&pad(prefix, 0), &pad(prefix, u64::MAX)) > 1
        }))
    }

    fn insert_at(&mut self, k: Key, n: usize, shares: &mut Option<bool>) -> Option<()> {
        let Some(root) = self.root.as_mut() else {
            self.root = Some(Arc::new(Node::Leaf(vec![k])));
            self.len = 1;
            *shares = Some(false);
            return Some(());
        };
        let split = Arc::make_mut(root).insert(k, n, shares)?;
        self.len += 1;
        if let Some(right) = split {
            let left = self.root.take().expect("the root is set");
            self.root = Some(Arc::new(Node::Branch(Branch {
                lasts: vec![left.last(), right.last()],
                counts: vec![left.total(), right.total()],
                children: vec![left, Arc::new(right)],
            })));
        }
        Some(())
    }

    /// Remove `k`; true if it was in the set. An absent key copies no node.
    pub fn remove(&mut self, k: &Key) -> bool {
        if !self.contains(k) {
            return false;
        }
        self.len -= 1;
        let root = self.root.as_mut().expect("a set holding a key has a root");
        Arc::make_mut(root).remove(k);
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
            stack: [(&[][..], 0); MAX_DEPTH],
            depth: 0,
            leaf: &[],
            pos: 0,
            hi: range.end_bound().cloned(),
        };
        let Some(root) = &self.root else {
            return it;
        };
        // descend to the leaf of the first key in the range, keeping the path
        let mut n: &Node = root;
        loop {
            match n {
                Node::Leaf(v) => {
                    it.leaf = v;
                    it.pos = match range.start_bound() {
                        Bound::Unbounded => 0,
                        Bound::Included(k) => v.partition_point(|x| x < k),
                        Bound::Excluded(k) => v.partition_point(|x| x <= k),
                    };
                    return it;
                }
                Node::Branch(b) => {
                    let i = match range.start_bound() {
                        Bound::Unbounded => 0,
                        Bound::Included(k) => b.lasts.partition_point(|l| l < k),
                        Bound::Excluded(k) => b.lasts.partition_point(|l| l <= k),
                    };
                    if i == b.children.len() {
                        // every key is before the range (only the root's descent can
                        // find this: a child taken holds a key not less than the start)
                        it.depth = 0;
                        return it;
                    }
                    it.stack[it.depth] = (&b.children, i);
                    it.depth += 1;
                    n = &b.children[i];
                }
            }
        }
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

/// The keys of a [`KeySet`] range, in order.
#[derive(Clone)]
pub struct Iter<'a> {
    /// the children of each branch on the path to the current leaf, and the index taken
    stack: [(&'a [Arc<Node>], usize); MAX_DEPTH],
    depth: usize,
    leaf: &'a [Key],
    pos: usize,
    hi: Bound<Key>,
}

impl<'a> Iter<'a> {
    /// Move to the first key of the next leaf; false at the end of the tree.
    fn next_leaf(&mut self) -> bool {
        // the deepest branch with a child after the one taken
        loop {
            if self.depth == 0 {
                return false;
            }
            let (children, i) = &mut self.stack[self.depth - 1];
            if *i + 1 < children.len() {
                *i += 1;
                break;
            }
            self.depth -= 1;
        }
        let (children, i) = self.stack[self.depth - 1];
        let mut n: &'a Node = &children[i];
        loop {
            match n {
                Node::Leaf(v) => {
                    self.leaf = v;
                    self.pos = 0;
                    return true;
                }
                Node::Branch(b) => {
                    self.stack[self.depth] = (&b.children, 0);
                    self.depth += 1;
                    n = &b.children[0];
                }
            }
        }
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
            self.leaf = &[];
            self.pos = 0;
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
            self.leaf = &[];
            self.pos = 0;
            self.depth = 0;
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
            self.leaf = &[];
            self.pos = 0;
            return None;
        }
        let k = &self.leaf[self.pos];
        if !self.before_end(k) {
            self.leaf = &[];
            self.pos = 0;
            self.depth = 0;
            return None;
        }
        self.pos += 1;
        Some(k)
    }
}

#[cfg(test)]
mod tests {
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
