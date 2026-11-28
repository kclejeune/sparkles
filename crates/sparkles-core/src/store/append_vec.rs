//! [`AppendVec`], a persistent vector that grows only at its end.
//!
//! The spatial index keeps the rows that commits add after its overlay was built in one
//! of these, and every snapshot's view holds its own. A clone shares everything, so a
//! commit's view starts as a clone of the last one and then appends its rows.
//!
//! The elements are kept in full chunks of [`CHUNK`] in a radix tree, and the last,
//! partly filled chunk in a buffer of its own. An append copies at most the buffer when a
//! clone shares it. Once the buffer fills, it moves into the tree, which copies the
//! branches on the path to the new chunk and never a chunk. A lookup descends one branch a
//! level, and iteration walks the chunks in order.

use std::ops::Index;
use std::sync::Arc;

/// Elements in a chunk, and children of a branch.
const CHUNK: usize = 32;
const BITS: u32 = CHUNK.trailing_zeros();
const MASK: usize = CHUNK - 1;

enum Node<T> {
    /// a full chunk, which is never changed once it is in the tree
    Chunk(Vec<T>),
    Branch(Vec<Arc<Node<T>>>),
}

impl<T: Clone> Clone for Node<T> {
    fn clone(&self) -> Self {
        match self {
            Node::Chunk(v) => Node::Chunk(v.clone()),
            Node::Branch(c) => {
                let mut children = Vec::with_capacity(CHUNK);
                children.extend(c.iter().cloned());
                Node::Branch(children)
            }
        }
    }
}

/// A persistent append-only vector (see the module docs).
pub struct AppendVec<T> {
    /// the full chunks, `None` while there are fewer than [`CHUNK`] elements
    root: Option<Arc<Node<T>>>,
    /// the levels of branches above the chunks
    height: u32,
    /// the elements after the full chunks, fewer than [`CHUNK`]
    tail: Arc<Vec<T>>,
    len: usize,
}

impl<T> Clone for AppendVec<T> {
    /// A clone shares every element with this vector.
    fn clone(&self) -> Self {
        AppendVec {
            root: self.root.clone(),
            height: self.height,
            tail: self.tail.clone(),
            len: self.len,
        }
    }
}

impl<T> Default for AppendVec<T> {
    fn default() -> Self {
        AppendVec {
            root: None,
            height: 0,
            tail: Arc::new(Vec::new()),
            len: 0,
        }
    }
}

impl<T> AppendVec<T> {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// The number of elements in full chunks.
    #[inline]
    fn chunked(&self) -> usize {
        self.len - self.tail.len()
    }

    /// The full chunk that holds element `i`, which is below [`chunked`](Self::chunked).
    fn chunk(&self, i: usize) -> &[T] {
        let mut n = self.root.as_deref().expect("a full chunk is in the tree");
        let mut level = self.height;
        loop {
            match n {
                Node::Chunk(v) => return v,
                Node::Branch(c) => {
                    n = &c[(i >> (BITS * level)) & MASK];
                    level -= 1;
                }
            }
        }
    }

    pub fn get(&self, i: usize) -> Option<&T> {
        if i >= self.len {
            None
        } else if i >= self.chunked() {
            Some(&self.tail[i - self.chunked()])
        } else {
            Some(&self.chunk(i)[i & MASK])
        }
    }

    /// The elements in order, a chunk at a time.
    pub fn chunks(&self) -> impl Iterator<Item = &[T]> + '_ {
        (0..self.chunked() / CHUNK)
            .map(move |c| self.chunk(c * CHUNK))
            .chain(std::iter::once(&self.tail[..]).filter(|t| !t.is_empty()))
    }

    /// The elements in order.
    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        self.chunks().flatten()
    }
}

impl<T: Clone> AppendVec<T> {
    /// Add `x` at the end.
    pub fn push_back(&mut self, x: T) {
        if Arc::get_mut(&mut self.tail).is_none() {
            // a clone shares the tail: copy it into a buffer with room for a full chunk
            let mut copy = Vec::with_capacity(CHUNK);
            copy.extend_from_slice(&self.tail);
            self.tail = Arc::new(copy);
        }
        let tail = Arc::get_mut(&mut self.tail).expect("the tail is not shared");
        if tail.capacity() < CHUNK {
            tail.reserve_exact(CHUNK - tail.len());
        }
        tail.push(x);
        self.len += 1;
        if tail.len() == CHUNK {
            // the next push allocates the buffer, so a view that ends at a full chunk
            // holds none
            let full = std::mem::take(tail);
            self.push_chunk(Arc::new(Node::Chunk(full)));
        }
    }

    /// Put `chunk` into the tree after the full chunks before it.
    fn push_chunk(&mut self, chunk: Arc<Node<T>>) {
        // the index of the chunk's first element
        let at = self.len - CHUNK;
        let Some(root) = self.root.as_mut() else {
            self.root = Some(chunk);
            return;
        };
        if at == CHUNK << (BITS * self.height) {
            // the tree is full: a new root over it and a path to the chunk
            let old = self.root.take().expect("the root is set");
            let mut children = Vec::with_capacity(CHUNK);
            children.extend([old, path(chunk, self.height)]);
            self.root = Some(Arc::new(Node::Branch(children)));
            self.height += 1;
            return;
        }
        let mut n = Arc::make_mut(root);
        let mut level = self.height;
        loop {
            let Node::Branch(c) = n else {
                unreachable!("a tree that is not full has a branch above its chunks")
            };
            let i = (at >> (BITS * level)) & MASK;
            if i == c.len() {
                c.push(path(chunk, level - 1));
                return;
            }
            n = Arc::make_mut(&mut c[i]);
            level -= 1;
        }
    }
}

/// `chunk` under `levels` new branches.
fn path<T>(chunk: Arc<Node<T>>, levels: u32) -> Arc<Node<T>> {
    (0..levels).fold(chunk, |n, _| {
        let mut children = Vec::with_capacity(CHUNK);
        children.push(n);
        Arc::new(Node::Branch(children))
    })
}

impl<T> Index<usize> for AppendVec<T> {
    type Output = T;
    fn index(&self, i: usize) -> &T {
        match self.get(i) {
            Some(x) => x,
            None => panic!("index {i} is past the end of a vector of {}", self.len),
        }
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for AppendVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<T: Clone> FromIterator<T> for AppendVec<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut v = AppendVec::new();
        for x in iter {
            v.push_back(x);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(v: &AppendVec<u64>, model: &[u64]) {
        assert_eq!(v.len(), model.len());
        assert!(v.iter().eq(model.iter()));
        for (i, x) in model.iter().enumerate() {
            assert_eq!(v[i], *x);
        }
        assert_eq!(v.get(model.len()), None);
        assert!(v.chunks().all(|c| !c.is_empty() && c.len() <= CHUNK));
    }

    #[test]
    fn holds_what_was_pushed_at_every_size() {
        let mut v = AppendVec::new();
        let mut model = Vec::new();
        check(&v, &model);
        // past three levels of branches: 32, 32^2, 32^3 elements and their neighbours
        for i in 0..(CHUNK.pow(3) + 3 * CHUNK + 5) as u64 {
            v.push_back(i * 7);
            model.push(i * 7);
            let n = model.len();
            if n < 4 * CHUNK
                || n.is_multiple_of(CHUNK)
                || (n - 1).is_multiple_of(CHUNK * CHUNK)
                || n.is_multiple_of(4099)
            {
                assert_eq!(v.len(), n);
                assert_eq!(v[n - 1], i * 7);
                assert_eq!(v.get(n), None);
            }
        }
        check(&v, &model);
        assert_eq!(v.height, 3);
    }

    #[test]
    fn clones_keep_their_elements_while_others_push() {
        let mut v: AppendVec<u64> = AppendVec::new();
        let mut versions = Vec::new();
        for i in 0..5_000u64 {
            if i % 37 == 0 {
                versions.push((v.clone(), i as usize));
            }
            v.push_back(i);
        }
        for (snap, n) in &versions {
            check(snap, &(0..*n as u64).collect::<Vec<_>>());
        }
        // two clones of one version that push different elements stay apart
        for (snap, n) in versions.iter().step_by(7) {
            let mut a = snap.clone();
            let mut b = snap.clone();
            let mut ma: Vec<u64> = (0..*n as u64).collect();
            let mut mb = ma.clone();
            for j in 0..(CHUNK as u64 * 3 + 1) {
                a.push_back(1_000_000 + j);
                ma.push(1_000_000 + j);
                if j % 2 == 0 {
                    b.push_back(2_000_000 + j);
                    mb.push(2_000_000 + j);
                }
            }
            check(&a, &ma);
            check(&b, &mb);
            check(snap, &(0..*n as u64).collect::<Vec<_>>());
        }
        check(&v, &(0..5_000).collect::<Vec<_>>());
    }

    #[test]
    fn an_append_to_a_clone_copies_no_full_chunk() {
        let base: AppendVec<u64> = (0..100_000).collect();
        let mut v = base.clone();
        for i in 0..(CHUNK as u64 * 2) {
            v.push_back(i);
        }
        // the chunks of the base are the same allocations in both
        for c in 0..base.chunked() / CHUNK {
            assert!(std::ptr::eq(base.chunk(c * CHUNK), v.chunk(c * CHUNK)));
        }
    }

    #[test]
    fn the_tail_holds_one_buffer_of_a_chunk() {
        let mut v: AppendVec<u64> = (0..CHUNK as u64).collect();
        // a vector that ends at a full chunk keeps no buffer for the next one
        assert_eq!(v.tail.capacity(), 0);
        v.push_back(1);
        assert_eq!(v.tail.capacity(), CHUNK);
        // an append to a clone copies the shared tail into a buffer of a chunk
        let snap = v.clone();
        v.push_back(2);
        assert!(!Arc::ptr_eq(&v.tail, &snap.tail));
        assert_eq!(v.tail.capacity(), CHUNK);
        assert_eq!(snap.tail.len(), 1);
        check(&v, &(0..CHUNK as u64).chain([1, 2]).collect::<Vec<_>>());
    }

    #[test]
    #[should_panic(expected = "past the end")]
    fn an_index_past_the_end_panics() {
        let v: AppendVec<u64> = (0..40).collect();
        let _ = v[40];
    }
}
