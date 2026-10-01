//! Packed R-trees: Hilbert-sorted, 16-entry nodes, `f32` boxes in the flatbush layout of
//! `geo-index`, read through that layout directly so that a tree held in memory and a
//! tree read from a file are searched by the same code.

use geo_index::indices::Indices;
use geo_index::rtree::sort::HilbertSort;
use geo_index::rtree::{RTreeBuilder, RTreeIndex, RTreeMetadata};

/// Entries per tree node.
pub(crate) const NODE_SIZE: u16 = 16;

/// A packed tree and the bytes it lives in.
pub(crate) struct PackedTree {
    data: TreeData,
    meta: RTreeMetadata<f32>,
}

/// Where a tree's bytes are (persisted trees will add a mapped file region).
enum TreeData {
    Owned(Vec<u8>),
}

impl PackedTree {
    /// Pack `boxes` (`None` without boxes); the tree's item `i` is the `i`-th box.
    pub fn pack(boxes: impl ExactSizeIterator<Item = [f32; 4]>) -> Option<PackedTree> {
        let n = boxes.len();
        if n == 0 {
            return None;
        }
        let mut b = RTreeBuilder::<f32>::new_with_node_size(n as u32, NODE_SIZE);
        for x in boxes {
            b.add(x[0], x[1], x[2], x[3]);
        }
        let t = b.finish::<HilbertSort>();
        let meta = t.metadata().clone();
        Some(PackedTree {
            data: TreeData::Owned(t.into_inner()),
            meta,
        })
    }

    /// The tree's bytes (the flatbush layout).
    pub fn data(&self) -> &[u8] {
        match &self.data {
            TreeData::Owned(v) => v,
        }
    }

    /// The tree's nodes.
    pub fn tree(&self) -> Tree<'_> {
        let d = self.data();
        Tree {
            boxes: self.meta.boxes_slice(d),
            indices: self.meta.indices_slice(d),
            items: self.meta.num_items() as usize * 4,
            node: self.meta.node_size() as usize * 4,
            bounds: self.meta.level_bounds(),
        }
    }

    /// Bytes held (the tree's buffer).
    pub fn bytes(&self) -> u64 {
        self.meta.data_buffer_length() as u64
    }

    /// Height of the tree.
    pub fn num_levels(&self) -> usize {
        self.meta.level_bounds().len()
    }

    /// The items whose box intersects one of the CRS84 `windows` (each once); counts the
    /// nodes visited in `nodes`.
    pub fn search(&self, windows: &[[f64; 4]], nodes: &mut u64) -> Vec<u32> {
        let ws: Vec<[f32; 4]> = windows.iter().map(super::index::window_f32).collect();
        let hits = |b: [f32; 4]| {
            ws.iter()
                .any(|w| b[0] <= w[2] && b[2] >= w[0] && b[1] <= w[3] && b[3] >= w[1])
        };
        let t = self.tree();
        let mut out = Vec::new();
        let root = t.root();
        if t.is_item(root) {
            if hits(t.bbox(root)) {
                out.push(t.item(root) as u32);
            }
            return out;
        }
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            *nodes += 1;
            for c in t.children(n) {
                if !hits(t.bbox(c)) {
                    continue;
                }
                if t.is_item(c) {
                    out.push(t.item(c) as u32);
                } else {
                    stack.push(c);
                }
            }
        }
        out
    }

    /// Estimated items whose box intersects one of `windows`: the subtree sizes of the
    /// intersecting nodes of the highest level with at least 256 nodes (an upper bound
    /// within one node per window boundary).
    pub fn estimate(&self, windows: &[[f64; 4]]) -> f64 {
        let t = self.tree();
        let n = self.meta.num_items() as usize;
        let ws: Vec<[f32; 4]> = windows.iter().map(super::index::window_f32).collect();
        let levels = self.num_levels();
        let mut level = 0;
        for l in (0..levels).rev() {
            if t.level_boxes(l).len() / 4 >= 256 {
                level = l;
                break;
            }
        }
        let boxes = t.level_boxes(level);
        let span = (NODE_SIZE as usize).saturating_pow(level as u32);
        let mut total = 0usize;
        for (j, b) in boxes.as_chunks::<4>().0.iter().enumerate() {
            let hit = ws
                .iter()
                .any(|w| b[0] <= w[2] && b[2] >= w[0] && b[1] <= w[3] && b[3] >= w[1]);
            if hit {
                let lo = j.saturating_mul(span);
                total += j.saturating_add(1).saturating_mul(span).min(n) - lo.min(n);
            }
        }
        total as f64
    }
}

/// A packed tree read through its layout: nodes are positions in `boxes` (four
/// coordinates each), level by level from the items up to the root; a node's index entry
/// is its first child's position, an item's its insertion index.
pub(crate) struct Tree<'a> {
    boxes: &'a [f32],
    indices: Indices<'a>,
    /// positions below this are items
    items: usize,
    node: usize,
    bounds: &'a [usize],
}

impl<'a> Tree<'a> {
    pub fn root(&self) -> usize {
        self.boxes.len() - 4
    }

    pub fn bbox(&self, pos: usize) -> [f32; 4] {
        [
            self.boxes[pos],
            self.boxes[pos + 1],
            self.boxes[pos + 2],
            self.boxes[pos + 3],
        ]
    }

    pub fn is_item(&self, pos: usize) -> bool {
        pos < self.items
    }

    /// The insertion index of the item at `pos`.
    pub fn item(&self, pos: usize) -> usize {
        self.indices.get(pos >> 2)
    }

    /// The positions of the children of the node at `pos`.
    pub fn children(&self, pos: usize) -> impl Iterator<Item = usize> + use<> {
        let start = self.indices.get(pos >> 2);
        // the end of the children's level
        let level_end = self
            .bounds
            .iter()
            .copied()
            .find(|&b| b > start)
            .unwrap_or(self.boxes.len());
        (start..(start + self.node).min(level_end)).step_by(4)
    }

    /// The boxes of level `l` (0: the items), four coordinates each.
    pub fn level_boxes(&self, l: usize) -> &'a [f32] {
        let lo = if l == 0 { 0 } else { self.bounds[l - 1] };
        &self.boxes[lo..self.bounds[l]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_geo_index() {
        let boxes: Vec<[f32; 4]> = (0..1000)
            .map(|i| {
                let (x, y) = ((i % 40) as f32, (i / 40) as f32);
                [x, y, x + 0.5, y + 0.5]
            })
            .collect();
        let t = PackedTree::pack(boxes.clone().into_iter()).unwrap();
        let bytes = t.data().to_vec();
        let r = geo_index::rtree::RTreeRef::<f32>::try_new(&bytes).unwrap();
        assert_eq!(t.num_levels(), r.num_levels());
        for l in 0..t.num_levels() {
            assert_eq!(t.tree().level_boxes(l), r.boxes_at_level(l).unwrap());
        }
        let mut nodes = 0;
        let mut a = t.search(&[[0.0, 0.0, 1.0, 1.0]], &mut nodes);
        let mut b = r.search(0.0, 0.0, 1.0, 1.0);
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);
        assert!(nodes > 0);
    }
}
