//! [`PMap`], a persistent ordered map: the B+ tree of [`KeySet`] with a value beside
//! each key and without its range counts.
//!
//! A clone shares every node, and a write copies only the nodes on the path from the root
//! to the leaf it changes, so a snapshot keeps its map while the writer goes on. A commit
//! that changes one entry of a map of a million entries therefore copies four or five
//! nodes, where a `BTreeMap` behind an `Arc` would copy the whole map. Leaves hold their
//! entries in one sorted array, and each branch keeps the largest key under each of its
//! children to route a descent.
//!
//! Keys and values that own heap memory, such as the byte strings of the control
//! keyspace, make a node's copy allocate once for each of them, so their maps get nodes
//! of half the size. Their keys also compare through a pointer, and a binary search over
//! them stops at an equal key.
//!
//! [`KeySet`]: super::keyset::KeySet

use super::keyset::{split_with_room, with_room};
use std::borrow::Borrow;
use std::cmp::Ordering;
use std::mem::{needs_drop, size_of};
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;

/// Whether the keys or the values of a map own heap memory.
const fn heavy<K, V>() -> bool {
    needs_drop::<K>() || needs_drop::<V>()
}

/// Most entries in a leaf.
const fn leaf_cap<K, V>() -> usize {
    if heavy::<K, V>() { 16 } else { 32 }
}

/// Most children of a branch.
const fn branch_cap<K, V>() -> usize {
    if heavy::<K, V>() { 16 } else { 32 }
}

/// The most levels of a tree. A node below the root holds at least a quarter of its
/// capacity, so a deeper tree would hold more than 4^31 entries.
const MAX_DEPTH: usize = 32;

enum Node<K, V> {
    Leaf(Vec<(K, V)>),
    Branch(Branch<K, V>),
}

struct Branch<K, V> {
    children: Vec<Arc<Node<K, V>>>,
    /// the largest key under each child
    lasts: Vec<K>,
}

impl<K: Clone, V: Clone> Clone for Node<K, V> {
    /// A copy of a node for a write, with room for the write's insert.
    fn clone(&self) -> Self {
        match self {
            Node::Leaf(v) => Node::Leaf(with_room(v, leaf_cap::<K, V>())),
            Node::Branch(b) => Node::Branch(Branch {
                children: with_room(&b.children, branch_cap::<K, V>()),
                lasts: with_room(&b.lasts, branch_cap::<K, V>()),
            }),
        }
    }
}

/// The shared allocation that holds a node, with its reference counts.
const fn node_header<K, V>() -> usize {
    2 * size_of::<usize>() + size_of::<Node<K, V>>()
}

/// The most bytes one node of a `PMap<K, V>` allocates, not counting what its keys and
/// values own on the heap. A merge can extend a node's vectors to twice the room a copy
/// gives before it splits again.
pub(crate) const fn node_bound<K, V>() -> usize {
    let leaf = 2 * (leaf_cap::<K, V>() + 1) * size_of::<(K, V)>();
    let branch = 2 * (branch_cap::<K, V>() + 1) * (size_of::<Arc<Node<K, V>>>() + size_of::<K>());
    node_header::<K, V>() + if leaf > branch { leaf } else { branch }
}

/// What an insert into a node did.
enum Inserted<K, V> {
    /// the key was there, and this was its value
    Replaced(V),
    /// the key is new: `grew` when it is now the node's largest key, and `split` the
    /// right half the node split off when it overflowed
    New {
        split: Option<Node<K, V>>,
        grew: bool,
    },
}

/// Whether keys of type `K` compare cheaply, as integers do, so that the standard
/// library's branchless search suits them.
const fn plain<K>() -> bool {
    !needs_drop::<K>() && size_of::<K>() <= 16
}

/// The position of the element of `s` that `cmp` finds equal, or where it would go.
/// Over keys that are not [`plain`] the search returns at the first equal element.
#[inline]
fn find<K, T>(s: &[T], cmp: impl Fn(&T) -> Ordering) -> Result<usize, usize> {
    if plain::<K>() {
        return s.binary_search_by(cmp);
    }
    let (mut lo, mut hi) = (0, s.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        match cmp(&s[mid]) {
            Ordering::Less => lo = mid + 1,
            Ordering::Greater => hi = mid,
            Ordering::Equal => return Ok(mid),
        }
    }
    Err(lo)
}

/// The number of leading elements of `s` that `before` holds for, when it holds for a
/// prefix of `s`.
#[inline]
fn lower<K, T>(s: &[T], before: impl Fn(&T) -> bool) -> usize {
    if plain::<K>() {
        return s.partition_point(before);
    }
    let (mut lo, mut hi) = (0, s.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if before(&s[mid]) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Where `k` falls among the sorted entries `v`.
#[inline]
fn search<K: Borrow<Q>, V, Q: Ord + ?Sized>(v: &[(K, V)], k: &Q) -> Result<usize, usize> {
    find::<K, _>(v, |e| e.0.borrow().cmp(k))
}

/// The position of an entry: the child taken at each branch from the root, and the
/// entry's place in its leaf.
struct Path {
    steps: [u8; MAX_DEPTH],
    depth: usize,
    pos: usize,
}

impl Path {
    fn steps(&self) -> &[u8] {
        &self.steps[..self.depth]
    }
}

impl<K, V> Branch<K, V> {
    /// The child whose range holds `k`: the first whose largest key is not less than
    /// `k`, or the last child.
    #[inline]
    fn route<Q: Ord + ?Sized>(&self, k: &Q) -> usize
    where
        K: Borrow<Q>,
    {
        let i = if plain::<K>() {
            self.lasts.partition_point(|l| l.borrow() < k)
        } else {
            match find::<K, _>(&self.lasts, |l| l.borrow().cmp(k)) {
                Ok(i) => i,
                Err(i) => i,
            }
        };
        i.min(self.children.len() - 1)
    }
}

impl<K: Ord + Clone, V: Clone> Branch<K, V> {
    /// The right half of a branch that holds more than its capacity.
    fn split(&mut self) -> Branch<K, V> {
        let at = self.children.len() / 2;
        Branch {
            children: split_with_room(&mut self.children, at, branch_cap::<K, V>()),
            lasts: split_with_room(&mut self.lasts, at, branch_cap::<K, V>()),
        }
    }
}

impl<K, V> Node<K, V> {
    fn last(&self) -> &K {
        match self {
            Node::Leaf(v) => &v.last().expect("a leaf in a tree is not empty").0,
            Node::Branch(b) => b.lasts.last().expect("a branch has children"),
        }
    }

    /// Entries: the pairs of a leaf, the children of a branch.
    fn width(&self) -> usize {
        match self {
            Node::Leaf(v) => v.len(),
            Node::Branch(b) => b.children.len(),
        }
    }

    /// The path to `k`, when the tree holds it.
    fn locate<Q: Ord + ?Sized>(&self, k: &Q) -> Option<Path>
    where
        K: Borrow<Q>,
    {
        let mut path = Path {
            steps: [0; MAX_DEPTH],
            depth: 0,
            pos: 0,
        };
        let mut n = self;
        loop {
            match n {
                Node::Leaf(v) => {
                    path.pos = search(v, k).ok()?;
                    return Some(path);
                }
                Node::Branch(b) => {
                    let i = b.route(k);
                    path.steps[path.depth] = i as u8;
                    path.depth += 1;
                    n = &b.children[i];
                }
            }
        }
    }

    fn get<Q: Ord + ?Sized>(&self, k: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        let mut n = self;
        loop {
            match n {
                Node::Leaf(v) => return search(v, k).ok().map(|i| &v[i].1),
                Node::Branch(b) => n = &b.children[b.route(k)],
            }
        }
    }
}

impl<K: Ord + Clone, V: Clone> Node<K, V> {
    /// The value at the end of `steps`, in the leaf's place `pos`, copying the nodes on
    /// its path that another map shares.
    fn value_at(&mut self, steps: &[u8], pos: usize) -> &mut V {
        match self {
            Node::Leaf(v) => &mut v[pos].1,
            Node::Branch(b) => {
                Arc::make_mut(&mut b.children[steps[0] as usize]).value_at(&steps[1..], pos)
            }
        }
    }

    /// Insert `k` with `value`.
    fn insert(&mut self, k: K, value: V) -> Inserted<K, V> {
        match self {
            Node::Leaf(v) => match search(v, &k) {
                Ok(at) => Inserted::Replaced(std::mem::replace(&mut v[at].1, value)),
                Err(at) => {
                    v.insert(at, (k, value));
                    let grew = at + 1 == v.len();
                    let split = (v.len() > leaf_cap::<K, V>()).then(|| {
                        let at = v.len() / 2;
                        Node::Leaf(split_with_room(v, at, leaf_cap::<K, V>()))
                    });
                    Inserted::New { split, grew }
                }
            },
            Node::Branch(b) => {
                let i = b.route(&k);
                let was_last = i + 1 == b.children.len();
                let (split, grew) = match Arc::make_mut(&mut b.children[i]).insert(k, value) {
                    Inserted::Replaced(old) => return Inserted::Replaced(old),
                    Inserted::New { split, grew } => (split, grew),
                };
                match split {
                    None => {
                        if grew {
                            b.lasts[i] = b.children[i].last().clone();
                        }
                    }
                    Some(right) => {
                        b.lasts[i] = b.children[i].last().clone();
                        b.lasts.insert(i + 1, right.last().clone());
                        b.children.insert(i + 1, Arc::new(right));
                    }
                }
                let split =
                    (b.children.len() > branch_cap::<K, V>()).then(|| Node::Branch(b.split()));
                Inserted::New {
                    split,
                    grew: grew && was_last,
                }
            }
        }
    }

    /// Remove the entry at the end of `steps`, in the leaf's place `pos`, and return its
    /// value and whether the node's largest key changed. A child left with fewer than a
    /// quarter of its capacity is merged with a neighbour, or the two share their entries
    /// evenly when together they are more than one node holds.
    fn remove_at(&mut self, steps: &[u8], pos: usize) -> (V, bool) {
        match self {
            Node::Leaf(v) => {
                let value = v.remove(pos).1;
                (value, pos == v.len())
            }
            Node::Branch(b) => {
                let i = steps[0] as usize;
                let child = Arc::make_mut(&mut b.children[i]);
                let (value, moved) = child.remove_at(&steps[1..], pos);
                let width = child.width();
                if width == 0 {
                    b.children.remove(i);
                    b.lasts.remove(i);
                    return (value, i == b.children.len());
                }
                if moved {
                    b.lasts[i] = child.last().clone();
                }
                let cap = match child {
                    Node::Leaf(_) => leaf_cap::<K, V>(),
                    Node::Branch(_) => branch_cap::<K, V>(),
                };
                let last = i + 1 == b.children.len();
                if width < cap / 4 && b.children.len() > 1 {
                    let j = if last { i - 1 } else { i };
                    // a merge keeps the larger bound of the pair, so the branch's
                    // largest key stays
                    rebalance(b, j, cap);
                }
                (value, moved && last)
            }
        }
    }
}

/// Merge children `j` and `j + 1` of `b`, or share their entries evenly when together
/// they are more than `cap`.
fn rebalance<K: Ord + Clone, V: Clone>(b: &mut Branch<K, V>, j: usize, cap: usize) {
    let right = Arc::unwrap_or_clone(b.children.remove(j + 1));
    let right_last = b.lasts.remove(j + 1);
    let left = Arc::make_mut(&mut b.children[j]);
    let split = match (left, right) {
        (Node::Leaf(l), Node::Leaf(r)) => {
            l.extend(r);
            let at = l.len() / 2;
            (l.len() > cap).then(|| Node::Leaf(split_with_room(l, at, cap)))
        }
        (Node::Branch(l), Node::Branch(r)) => {
            l.children.extend(r.children);
            l.lasts.extend(r.lasts);
            (l.children.len() > cap).then(|| Node::Branch(l.split()))
        }
        _ => unreachable!("siblings are at the same level"),
    };
    match split {
        None => b.lasts[j] = right_last,
        Some(r) => {
            b.lasts[j] = b.children[j].last().clone();
            b.lasts.insert(j + 1, right_last);
            b.children.insert(j + 1, Arc::new(r));
        }
    }
}

/// A persistent ordered map (see the module docs).
pub struct PMap<K, V> {
    root: Option<Arc<Node<K, V>>>,
    len: usize,
}

impl<K, V> Clone for PMap<K, V> {
    /// A clone shares every node with this map.
    fn clone(&self) -> Self {
        PMap {
            root: self.root.clone(),
            len: self.len,
        }
    }
}

impl<K, V> Default for PMap<K, V> {
    fn default() -> Self {
        PMap { root: None, len: 0 }
    }
}

impl<K, V> PMap<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether `other` is this map or an unchanged clone of it: both are empty, or both
    /// share one root. A map changed after a clone has a copied root.
    pub fn same(&self, other: &Self) -> bool {
        self.len == other.len
            && match (&self.root, &other.root) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }

    pub fn get<Q: Ord + ?Sized>(&self, k: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        self.root.as_ref()?.get(k)
    }

    pub fn contains_key<Q: Ord + ?Sized>(&self, k: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.get(k).is_some()
    }

    fn locate<Q: Ord + ?Sized>(&self, k: &Q) -> Option<Path>
    where
        K: Borrow<Q>,
    {
        self.root.as_ref()?.locate(k)
    }

    fn empty_iter(&self) -> Iter<'_, K, V> {
        Iter {
            stack: Vec::new(),
            leaf: &[],
            pos: 0,
            end: None,
        }
    }

    /// The entries in key order.
    pub fn iter(&self) -> Iter<'_, K, V> {
        let mut it = self.empty_iter();
        if let Some(r) = &self.root {
            it.descend(r, |_| false);
        }
        it
    }

    /// The entries whose keys lie in `range`, in key order. A range whose start comes
    /// after its end is empty.
    pub fn range<Q: Ord + ?Sized, R: RangeBounds<Q>>(&self, range: R) -> Iter<'_, K, V>
    where
        K: Borrow<Q>,
    {
        let mut it = self.empty_iter();
        let empty = match (range.start_bound(), range.end_bound()) {
            (Bound::Included(a), Bound::Included(b)) => a > b,
            (Bound::Included(a) | Bound::Excluded(a), Bound::Excluded(b) | Bound::Included(b)) => {
                a >= b
            }
            _ => false,
        };
        let Some(root) = self.root.as_ref().filter(|_| !empty) else {
            return it;
        };
        match range.start_bound() {
            Bound::Unbounded => it.descend(root, |_| false),
            Bound::Included(lo) => it.descend(root, |x| x.borrow() < lo),
            Bound::Excluded(lo) => it.descend(root, |x| x.borrow() <= lo),
        }
        it.end = match range.end_bound() {
            Bound::Unbounded => None,
            Bound::Included(hi) => seek(root, |x| x.borrow() <= hi),
            Bound::Excluded(hi) => seek(root, |x| x.borrow() < hi),
        };
        it
    }
}

impl<K: Ord + Clone, V: Clone> PMap<K, V> {
    /// Set `k` to `value`, and return the value it had. The descent copies the nodes on
    /// its path that a clone shares.
    pub fn insert(&mut self, k: K, value: V) -> Option<V> {
        let Some(root) = self.root.as_mut() else {
            self.root = Some(Arc::new(Node::Leaf(with_room(
                &[(k, value)],
                leaf_cap::<K, V>(),
            ))));
            self.len = 1;
            return None;
        };
        let split = match Arc::make_mut(root).insert(k, value) {
            Inserted::Replaced(old) => return Some(old),
            Inserted::New { split, .. } => split,
        };
        self.len += 1;
        if let Some(right) = split {
            let left = self.root.take().expect("the root is set");
            let mut lasts = Vec::with_capacity(branch_cap::<K, V>() + 1);
            lasts.extend([left.last().clone(), right.last().clone()]);
            let mut children = Vec::with_capacity(branch_cap::<K, V>() + 1);
            children.extend([left, Arc::new(right)]);
            self.root = Some(Arc::new(Node::Branch(Branch { children, lasts })));
        }
        None
    }

    /// The value at `path`, copying the nodes on it that another map shares.
    fn value_at(&mut self, path: &Path) -> &mut V {
        Arc::make_mut(self.root.as_mut().expect("a map with a path has a root"))
            .value_at(path.steps(), path.pos)
    }

    /// The value of `k`, for a change. A key that is not in the map copies no node, and
    /// the copy of the path to one that is compares no keys.
    pub fn get_mut<Q: Ord + ?Sized>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        let path = self.locate(k)?;
        Some(self.value_at(&path))
    }

    /// The value of `k`, inserted with `make` when the map does not hold `k`.
    pub fn get_or_insert_with(&mut self, k: K, make: impl FnOnce() -> V) -> &mut V {
        let path = match self.locate(&k) {
            Some(path) => path,
            None => {
                self.insert(k.clone(), make());
                self.locate(&k).expect("the key was inserted")
            }
        };
        self.value_at(&path)
    }

    /// Remove `k` and return its value. A key that is not in the map copies no node.
    pub fn remove<Q: Ord + ?Sized>(&mut self, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        let path = self.locate(k)?;
        self.len -= 1;
        let root = self.root.as_mut().expect("a map with a key has a root");
        let (value, _) = Arc::make_mut(root).remove_at(path.steps(), path.pos);
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
        Some(value)
    }
}

/// The leaf of the first entry under `n` whose key `before` is false for, and the
/// entry's position in it (`before` holds for a prefix of the keys in order). `None` when
/// it holds for every key.
fn seek<K, V>(mut n: &Node<K, V>, before: impl Fn(&K) -> bool) -> Option<(&[(K, V)], usize)> {
    loop {
        match n {
            Node::Leaf(v) => {
                let pos = lower::<K, _>(v, |e| before(&e.0));
                return (pos < v.len()).then_some((&v[..], pos));
            }
            Node::Branch(b) => n = b.children.get(lower::<K, _>(&b.lasts, &before))?,
        }
    }
}

/// The children of a branch, as an iterator holds them.
type Children<'a, K, V> = &'a [Arc<Node<K, V>>];

/// The entries of a [`PMap`] range, in order. It keeps the path from the root to its
/// leaf, so stepping to the next leaf compares no keys.
pub struct Iter<'a, K, V> {
    /// for each branch above the leaf, its children and the position of the child the
    /// walk is in
    stack: Vec<(Children<'a, K, V>, usize)>,
    leaf: &'a [(K, V)],
    pos: usize,
    /// the leaf and position of the first entry past the range, `None` for the end of
    /// the map
    end: Option<(&'a [(K, V)], usize)>,
}

impl<'a, K, V> Iter<'a, K, V> {
    /// Descend from `n` to the first entry whose key `before` is false for, keeping the
    /// path. The walk ends at once when there is none.
    fn descend(&mut self, mut n: &'a Node<K, V>, before: impl Fn(&K) -> bool) {
        loop {
            match n {
                Node::Leaf(v) => {
                    self.leaf = v;
                    self.pos = lower::<K, _>(v, |e| before(&e.0));
                    if self.pos == v.len() {
                        self.finish();
                    }
                    return;
                }
                Node::Branch(b) => {
                    let i = lower::<K, _>(&b.lasts, &before);
                    if i == b.children.len() {
                        self.finish();
                        return;
                    }
                    self.stack.push((&b.children, i));
                    n = &b.children[i];
                }
            }
        }
    }

    fn finish(&mut self) {
        self.stack.clear();
        self.leaf = &[];
        self.pos = 0;
        self.end = None;
    }

    /// Move to the first entry of the next leaf; false at the end of the map.
    fn next_leaf(&mut self) -> bool {
        while let Some((children, i)) = self.stack.last_mut() {
            *i += 1;
            if *i < children.len() {
                let mut n: &'a Node<K, V> = &children[*i];
                loop {
                    match n {
                        Node::Leaf(v) => {
                            (self.leaf, self.pos) = (v, 0);
                            return true;
                        }
                        Node::Branch(b) => {
                            self.stack.push((&b.children, 0));
                            n = &b.children[0];
                        }
                    }
                }
            }
            self.stack.pop();
        }
        self.finish();
        false
    }
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    #[inline]
    fn next(&mut self) -> Option<(&'a K, &'a V)> {
        if self.pos >= self.leaf.len() && !self.next_leaf() {
            return None;
        }
        if let Some((leaf, pos)) = self.end
            && std::ptr::eq(leaf, self.leaf)
            && self.pos >= pos
        {
            self.finish();
            return None;
        }
        let (k, v) = &self.leaf[self.pos];
        self.pos += 1;
        Some((k, v))
    }
}

impl<'a, K, V> IntoIterator for &'a PMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;
    fn into_iter(self) -> Iter<'a, K, V> {
        self.iter()
    }
}

impl<K: std::fmt::Debug, V: std::fmt::Debug> std::fmt::Debug for PMap<K, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: PartialEq, V: PartialEq> PartialEq for PMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.same(other) || (self.len == other.len && self.iter().eq(other.iter()))
    }
}

impl<K: Eq, V: Eq> Eq for PMap<K, V> {}

impl<K: Ord + Clone, V: Clone> FromIterator<(K, V)> for PMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut m = PMap::new();
        for (k, v) in iter {
            m.insert(k, v);
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            // splitmix64
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Check the tree's invariants under `n` and return its number of entries: sorted
    /// keys, bounds that are each child's largest key, nodes within their capacity and,
    /// below the root, never empty, and leaves all at one depth.
    fn sane<K: Ord + Clone + std::fmt::Debug, V>(
        n: &Node<K, V>,
        root: bool,
        depth: usize,
        leaf_depth: &mut Option<usize>,
    ) -> usize {
        match n {
            Node::Leaf(v) => {
                assert!(v.len() <= leaf_cap::<K, V>());
                assert!(root || !v.is_empty());
                assert!(v.windows(2).all(|w| w[0].0 < w[1].0));
                assert_eq!(*leaf_depth.get_or_insert(depth), depth);
                v.len()
            }
            Node::Branch(b) => {
                assert!(b.children.len() <= branch_cap::<K, V>());
                assert!(b.children.len() >= if root { 2 } else { 1 });
                assert_eq!(b.children.len(), b.lasts.len());
                assert!(b.lasts.windows(2).all(|w| w[0] < w[1]));
                let mut total = 0;
                for (c, l) in b.children.iter().zip(&b.lasts) {
                    assert_eq!(c.last(), l);
                    total += sane(c, false, depth + 1, leaf_depth);
                }
                total
            }
        }
    }

    fn check<K, V>(m: &PMap<K, V>, model: &BTreeMap<K, V>)
    where
        K: Ord + Clone + std::fmt::Debug,
        V: PartialEq + std::fmt::Debug,
    {
        assert_eq!(m.len(), model.len());
        assert_eq!(m.is_empty(), model.is_empty());
        if let Some(r) = &m.root {
            assert_eq!(sane(r, true, 0, &mut None), m.len());
        }
        assert!(m.iter().eq(model.iter()));
    }

    #[test]
    fn an_empty_map_has_nothing() {
        let m: PMap<u64, u64> = PMap::new();
        assert!(m.is_empty());
        assert_eq!(m.get(&1), None);
        assert_eq!(m.iter().count(), 0);
        assert_eq!(m.range(3..).count(), 0);
        assert_eq!(m, PMap::default());
    }

    #[test]
    fn matches_an_ordered_map_under_random_writes() {
        for (seed, span) in [(1u64, 50u64), (2, 2_000), (3, 100_000)] {
            let mut rng = Rng(seed);
            let mut m: PMap<u64, u64> = PMap::new();
            let mut model = BTreeMap::new();
            let mut versions: Vec<(PMap<u64, u64>, BTreeMap<u64, u64>)> = Vec::new();
            for step in 0..20_000u64 {
                let k = rng.below(span);
                match rng.below(10) {
                    0..=4 => assert_eq!(m.insert(k, step), model.insert(k, step)),
                    5..=7 => assert_eq!(m.remove(&k), model.remove(&k)),
                    8 => {
                        let a = m.get_mut(&k).map(|v| {
                            *v += 1;
                            *v
                        });
                        let b = model.get_mut(&k).map(|v| {
                            *v += 1;
                            *v
                        });
                        assert_eq!(a, b);
                    }
                    _ => {
                        *m.get_or_insert_with(k, || 7) += 1;
                        *model.entry(k).or_insert(7) += 1;
                    }
                }
                assert_eq!(m.get(&k), model.get(&k));
                if step % 997 == 0 {
                    check(&m, &model);
                    versions.push((m.clone(), model.clone()));
                }
                if step % 211 == 0 {
                    let (a, b) = (rng.below(span + 2), rng.below(span + 2));
                    let (lo, hi) = (a.min(b), a.max(b));
                    assert!(m.range(lo..hi).eq(model.range(lo..hi)));
                    assert!(m.range(lo..=hi).eq(model.range(lo..=hi)));
                    assert!(m.range(lo..).eq(model.range(lo..)));
                    assert!(m.range(..hi).eq(model.range(..hi)));
                    assert!(
                        m.range((Bound::Excluded(lo), Bound::Included(hi)))
                            .eq(model.range((Bound::Excluded(lo), Bound::Included(hi))))
                    );
                }
            }
            check(&m, &model);
            // every snapshot kept its entries while the writer went on
            for (snap, model) in &versions {
                check(snap, model);
            }
            // and removing everything leaves an empty map
            let keys: Vec<u64> = model.keys().copied().collect();
            for k in keys {
                assert!(m.remove(&k).is_some());
                model.remove(&k);
            }
            check(&m, &model);
            assert!(m.root.is_none());
        }
    }

    #[test]
    fn maps_with_heap_keys_match_an_ordered_map() {
        // smaller nodes, and the search that stops at an equal key
        assert!(heavy::<String, Box<u64>>() && leaf_cap::<String, Box<u64>>() == 16);
        let mut rng = Rng(11);
        let mut m: PMap<String, Box<u64>> = PMap::new();
        let mut model: BTreeMap<String, Box<u64>> = BTreeMap::new();
        let mut versions = Vec::new();
        for step in 0..30_000u64 {
            let k = format!("{:05}", rng.below(3_000));
            match rng.below(8) {
                0..=3 => assert_eq!(
                    m.insert(k.clone(), Box::new(step)),
                    model.insert(k.clone(), Box::new(step))
                ),
                4..=6 => assert_eq!(m.remove(k.as_str()), model.remove(k.as_str())),
                _ => {
                    **m.get_or_insert_with(k.clone(), || Box::new(0)) += 1;
                    **model.entry(k.clone()).or_insert_with(|| Box::new(0)) += 1;
                }
            }
            assert_eq!(m.get(k.as_str()), model.get(k.as_str()));
            if step % 1_009 == 0 {
                check(&m, &model);
                versions.push((m.clone(), model.clone()));
                let lo = format!("{:05}", rng.below(3_000));
                let from = (Bound::Included(lo.as_str()), Bound::Unbounded);
                assert!(
                    m.range::<str, _>(from)
                        .take(40)
                        .eq(model.range::<str, _>(from).take(40))
                );
            }
        }
        check(&m, &model);
        for (snap, model) in &versions {
            check(snap, model);
        }
    }

    #[test]
    fn byte_keys_look_up_by_slice() {
        let mut m: PMap<Box<[u8]>, Arc<[u8]>> = PMap::new();
        let mut model: BTreeMap<Box<[u8]>, Arc<[u8]>> = BTreeMap::new();
        let mut rng = Rng(9);
        for i in 0..5_000u64 {
            let k: Box<[u8]> = format!("k/{:x}/{}", rng.below(64), rng.below(500))
                .into_bytes()
                .into();
            let v: Arc<[u8]> = i.to_le_bytes().as_slice().into();
            if rng.below(4) == 0 {
                assert_eq!(m.remove(&*k), model.remove(&*k));
            } else {
                assert_eq!(m.insert(k.clone(), v.clone()), model.insert(k, v));
            }
        }
        check(&m, &model);
        for p in ["k/1", "k/a/", "k/ff", "k/", "z", ""] {
            let start: &[u8] = p.as_bytes();
            let a: Vec<_> = m
                .range::<[u8], _>((Bound::Included(start), Bound::Unbounded))
                .take_while(|(k, _)| k.starts_with(start))
                .collect();
            let b: Vec<_> = model
                .range::<[u8], _>((Bound::Included(start), Bound::Unbounded))
                .take_while(|(k, _)| k.starts_with(start))
                .collect();
            assert_eq!(a, b);
        }
    }

    #[test]
    fn a_range_past_the_end_or_reversed_is_empty() {
        let m: PMap<u64, ()> = (0..1_000).map(|k| (k * 2, ())).collect();
        assert_eq!(m.range(5_000..).count(), 0);
        assert_eq!(m.range(1_999..).count(), 0);
        assert_eq!(m.range(1_998..).count(), 1);
        assert_eq!(m.range(10..10).count(), 0);
        assert_eq!(m.range(10..=10).count(), 1);
        assert_eq!(
            m.range((Bound::Excluded(10), Bound::Excluded(10))).count(),
            0
        );
        #[allow(clippy::reversed_empty_ranges)]
        let reversed = m.range(20..10).count();
        assert_eq!(reversed, 0);
        assert_eq!(m.range(..0).count(), 0);
        assert_eq!(m.range(..=0).count(), 1);
    }

    /// The nodes of `m` that no other map shares.
    fn unshared<K, V>(m: &PMap<K, V>) -> usize {
        let mut n = 0;
        let mut stack: Vec<&Arc<Node<K, V>>> = m.root.iter().collect();
        while let Some(node) = stack.pop() {
            if Arc::strong_count(node) != 1 {
                continue;
            }
            n += 1;
            if let Node::Branch(b) = &**node {
                stack.extend(&b.children);
            }
        }
        n
    }

    fn depth<K, V>(m: &PMap<K, V>) -> usize {
        let mut d = 0;
        let mut n = m.root.as_deref();
        while let Some(node) = n {
            d += 1;
            n = match node {
                Node::Leaf(_) => None,
                Node::Branch(b) => Some(&b.children[0]),
            };
        }
        d
    }

    #[test]
    fn a_write_to_a_clone_copies_one_path() {
        let base: PMap<u64, u64> = (0..1_000_000u64).map(|k| (k * 3, k)).collect();
        let d = depth(&base);
        assert!(d >= 4, "a million entries make a tree of {d} levels");
        let mut rng = Rng(5);
        for _ in 0..100 {
            let k = rng.below(3_000_000);
            // an update, an insert of a new key and a removal each copy at most the
            // nodes of one root-to-leaf path (a split or merge adds a node per level)
            let mut m = base.clone();
            m.insert(k / 3 * 3, 0);
            assert_eq!(unshared(&m), d);
            let mut m = base.clone();
            if m.insert(k / 3 * 3 + 1, 0).is_none() {
                assert!(unshared(&m) <= 2 * d + 1);
            }
            let mut m = base.clone();
            m.remove(&(k / 3 * 3));
            assert!(unshared(&m) <= 2 * d);
            let mut m = base.clone();
            *m.get_mut(&(k / 3 * 3)).unwrap() += 1;
            assert_eq!(unshared(&m), d);
            assert_eq!(base.get(&(k / 3 * 3)), Some(&(k / 3)));
        }
        assert_eq!(base.len(), 1_000_000);
        assert!(
            base.iter()
                .map(|(k, _)| *k)
                .eq((0..1_000_000u64).map(|k| k * 3))
        );
    }

    #[test]
    fn a_missing_key_copies_nothing() {
        let base: PMap<u64, u64> = (0..10_000u64).map(|k| (k * 2, k)).collect();
        let mut m = base.clone();
        assert_eq!(m.remove(&1), None);
        assert_eq!(m.get_mut(&3), None);
        assert_eq!(unshared(&m), 0);
        assert!(m.same(&base));
    }

    #[test]
    fn writes_to_two_clones_stay_apart() {
        let base: PMap<u64, u64> = (0..5_000u64).map(|k| (k, k)).collect();
        let mut a = base.clone();
        let mut b = base.clone();
        for k in 0..2_500u64 {
            a.remove(&(k * 2));
            b.insert(k * 2, 0);
            b.insert(10_000 + k, 1);
        }
        assert_eq!(base.len(), 5_000);
        assert!(base.iter().all(|(k, v)| k == v));
        assert_eq!(a.len(), 2_500);
        assert!(a.iter().all(|(k, _)| k % 2 == 1));
        assert_eq!(b.len(), 7_500);
        assert!(
            b.iter()
                .take(5_000)
                .all(|(k, v)| if k % 2 == 0 { *v == 0 } else { k == v })
        );
        assert_ne!(a, b);
        assert_eq!(base, base.clone());
        let rebuilt: PMap<u64, u64> = base.iter().map(|(k, v)| (*k, *v)).collect();
        assert_eq!(base, rebuilt);
        assert!(!base.same(&rebuilt));
    }

    #[test]
    fn sequential_and_reverse_fills_split_and_merge() {
        for reverse in [false, true] {
            let mut m: PMap<u32, u32> = PMap::new();
            let mut model = BTreeMap::new();
            let keys: Vec<u32> = if reverse {
                (0..50_000).rev().collect()
            } else {
                (0..50_000).collect()
            };
            for &k in &keys {
                m.insert(k, k);
                model.insert(k, k);
            }
            check(&m, &model);
            // drain from the middle out, which merges and redistributes at every level
            for &k in keys.iter().skip(10_000).take(35_000) {
                assert_eq!(m.remove(&k), model.remove(&k));
            }
            check(&m, &model);
        }
    }
}
