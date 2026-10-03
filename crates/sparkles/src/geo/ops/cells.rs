//! A cell grid over a region, which decides most tests of small geometries against it
//! without the exact computation.
//!
//! The grid covers the region's envelope. A cell that an edge of the region passes
//! through, or comes close to, is a boundary cell. Every other cell is entirely in the
//! region's interior or entirely outside it: cells connected without crossing a boundary
//! cell are on the same side, so one point-in-polygon test per connected group decides
//! them all. A geometry whose envelope covers only interior cells lies in the region's
//! interior, and one whose envelope covers only outside cells (or lies off the grid) is
//! disjoint from it. Anything else needs the exact test.
//!
//! The idea is QLever's, whose spatial joins approximate regions by grid cells; nothing
//! of its code is used.

use georust::{Coord, Geometry, Intersects, LineString, Polygon};

/// Regions with fewer vertices are tested exactly: their tests are cheap already.
pub const MIN_VERTICES: u32 = 32;
/// Cells per axis, at most.
const MAX_SIDE: usize = 256;
/// Cells per axis, at least.
const MIN_SIDE: usize = 8;

const OUTSIDE: u8 = 0;
const INSIDE: u8 = 1;
const BOUNDARY: u8 = 2;
const UNSET: u8 = 3;

/// Where a geometry's envelope lies relative to the region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// in the region's interior
    Inside,
    /// disjoint from the region
    Outside,
}

/// The cells of one region.
pub struct CellGrid {
    min: Coord<f64>,
    /// cell width and height
    w: f64,
    h: f64,
    nx: usize,
    ny: usize,
    cells: Vec<u8>,
}

impl CellGrid {
    /// The grid of a polygon or multipolygon with at least [`MIN_VERTICES`] vertices
    /// (`None` for other geometries, or a degenerate envelope).
    pub fn build(g: &Geometry<f64>, vertices: u32) -> Option<CellGrid> {
        if vertices < MIN_VERTICES {
            return None;
        }
        let polys: Vec<&Polygon<f64>> = match g {
            Geometry::Polygon(p) => vec![p],
            Geometry::MultiPolygon(m) => m.0.iter().collect(),
            _ => return None,
        };
        let rect = georust::BoundingRect::bounding_rect(g)?;
        let (min, max) = (rect.min(), rect.max());
        let (dx, dy) = (max.x - min.x, max.y - min.y);
        if !(dx > 0.0 && dy > 0.0 && dx.is_finite() && dy.is_finite()) {
            return None;
        }
        // 16 cells per vertex, in the envelope's proportions: a few boundary cells per
        // edge, most cells decided
        let n = (16.0 * f64::from(vertices)).clamp(256.0, (MAX_SIDE * MAX_SIDE) as f64);
        let aspect = dx / dy;
        let nx = ((n * aspect).sqrt().round() as usize).clamp(MIN_SIDE, MAX_SIDE);
        let ny = ((n / aspect).sqrt().round() as usize).clamp(MIN_SIDE, MAX_SIDE);
        let mut grid = CellGrid {
            min,
            w: dx / nx as f64,
            h: dy / ny as f64,
            nx,
            ny,
            cells: vec![UNSET; nx * ny],
        };
        for p in &polys {
            grid.mark_ring(p.exterior());
            for r in p.interiors() {
                grid.mark_ring(r);
            }
        }
        grid.fill(g);
        Some(grid)
    }

    /// The cell columns or rows that the interval `[lo, hi]` of one axis touches, with
    /// a margin of a millionth of a cell on each side.
    fn span(lo: f64, hi: f64, origin: f64, size: f64, n: usize) -> Option<(usize, usize)> {
        let eps = 1e-6;
        let a = ((lo - origin) / size - eps).floor();
        let b = ((hi - origin) / size + eps).floor();
        if !(a.is_finite() && b.is_finite()) || b < 0.0 || a >= n as f64 {
            return None;
        }
        Some((a.max(0.0) as usize, (b as usize).min(n - 1)))
    }

    /// Mark the cells every edge of `ring` passes through as boundary cells: per column
    /// the edge crosses, the rows of the part of the edge in that column.
    fn mark_ring(&mut self, ring: &LineString<f64>) {
        for l in ring.lines() {
            let (a, b) = (l.start, l.end);
            let Some((c0, c1)) =
                Self::span(a.x.min(b.x), a.x.max(b.x), self.min.x, self.w, self.nx)
            else {
                continue;
            };
            for c in c0..=c1 {
                // the edge's extent in y within column c
                let (x0, x1) = (
                    self.min.x + c as f64 * self.w,
                    self.min.x + (c + 1) as f64 * self.w,
                );
                let (ylo, yhi) = if a.x == b.x {
                    (a.y.min(b.y), a.y.max(b.y))
                } else {
                    let at = |x: f64| a.y + (b.y - a.y) * ((x - a.x) / (b.x - a.x));
                    let xa = x0.max(a.x.min(b.x));
                    let xb = x1.min(a.x.max(b.x));
                    let (ya, yb) = (at(xa), at(xb));
                    (ya.min(yb), ya.max(yb))
                };
                if let Some((r0, r1)) = Self::span(ylo, yhi, self.min.y, self.h, self.ny) {
                    for r in r0..=r1 {
                        self.cells[r * self.nx + c] = BOUNDARY;
                    }
                }
            }
        }
    }

    /// Classify the other cells: each group of cells connected without a boundary cell
    /// by one test of its first cell's centre.
    fn fill(&mut self, g: &Geometry<f64>) {
        let mut stack = Vec::new();
        for start in 0..self.cells.len() {
            if self.cells[start] != UNSET {
                continue;
            }
            let (c, r) = (start % self.nx, start / self.nx);
            let centre = Coord {
                x: self.min.x + (c as f64 + 0.5) * self.w,
                y: self.min.y + (r as f64 + 0.5) * self.h,
            };
            let side = if g.intersects(&centre) {
                INSIDE
            } else {
                OUTSIDE
            };
            self.cells[start] = side;
            stack.push(start);
            while let Some(i) = stack.pop() {
                let (c, r) = (i % self.nx, i / self.nx);
                let mut visit = |j: usize| {
                    if self.cells[j] == UNSET {
                        self.cells[j] = side;
                        stack.push(j);
                    }
                };
                if c > 0 {
                    visit(i - 1);
                }
                if c + 1 < self.nx {
                    visit(i + 1);
                }
                if r > 0 {
                    visit(i - self.nx);
                }
                if r + 1 < self.ny {
                    visit(i + self.nx);
                }
            }
        }
    }

    /// Where the envelope `[minx, miny, maxx, maxy]` lies, when the cells decide it.
    pub fn side(&self, b: [f64; 4]) -> Option<Side> {
        if b.iter().any(|v| v.is_nan()) {
            return None;
        }
        let max_x = self.min.x + self.nx as f64 * self.w;
        let max_y = self.min.y + self.ny as f64 * self.h;
        let off_grid = b[0] < self.min.x || b[1] < self.min.y || b[2] > max_x || b[3] > max_y;
        let (Some((c0, c1)), Some((r0, r1))) = (
            Self::span(b[0], b[2], self.min.x, self.w, self.nx),
            Self::span(b[1], b[3], self.min.y, self.h, self.ny),
        ) else {
            // entirely off the grid, so outside the region's envelope
            return Some(Side::Outside);
        };
        let mut seen = [false; 3];
        for r in r0..=r1 {
            for c in c0..=c1 {
                let k = self.cells[r * self.nx + c];
                if k == BOUNDARY {
                    return None;
                }
                seen[k as usize] = true;
                if seen[OUTSIDE as usize] && seen[INSIDE as usize] {
                    return None;
                }
            }
        }
        if seen[INSIDE as usize] {
            (!off_grid).then_some(Side::Inside)
        } else {
            Some(Side::Outside)
        }
    }

    /// Cells of each kind: (outside, inside, boundary).
    #[cfg(test)]
    fn counts(&self) -> (usize, usize, usize) {
        let n = |k: u8| self.cells.iter().filter(|&&c| c == k).count();
        (n(OUTSIDE), n(INSIDE), n(BOUNDARY))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use georust::{Point, Relate};

    /// A star with `n` points and a square hole, as a polygon of `2n` vertices.
    fn star(n: usize) -> Geometry<f64> {
        let mut ring: Vec<Coord<f64>> = (0..2 * n)
            .map(|i| {
                let a = std::f64::consts::PI * i as f64 / n as f64;
                let r = if i % 2 == 0 { 10.0 } else { 4.0 };
                Coord {
                    x: r * a.cos(),
                    y: r * a.sin(),
                }
            })
            .collect();
        ring.push(ring[0]);
        let hole = LineString::from(vec![
            (-1.0, -1.0),
            (-1.0, 1.0),
            (1.0, 1.0),
            (1.0, -1.0),
            (-1.0, -1.0),
        ]);
        Geometry::Polygon(Polygon::new(LineString::from(ring), vec![hole]))
    }

    #[test]
    fn small_and_non_areal_geometries_have_no_grid() {
        assert!(CellGrid::build(&star(4), 9).is_none());
        let line = Geometry::LineString(LineString::from(vec![(0.0, 0.0); 40]));
        assert!(CellGrid::build(&line, 40).is_none());
    }

    #[test]
    fn the_cells_agree_with_the_exact_test() {
        let g = star(40);
        let grid = CellGrid::build(&g, 81).unwrap();
        let (outside, inside, boundary) = grid.counts();
        assert!(outside > 0 && inside > 0 && boundary > 0);
        // a deterministic spread of points and small boxes over and around the star
        let mut decided = 0;
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % 26_000) as f64 / 1000.0 - 13.0
        };
        for i in 0..20_000 {
            let (px, py) = (next(), next());
            let size = if i % 2 == 0 { 0.0 } else { 0.2 };
            let b = [px, py, px + size, py + size];
            let Some(side) = grid.side(b) else { continue };
            decided += 1;
            let probe: Geometry<f64> = if size == 0.0 {
                Point::new(px, py).into()
            } else {
                georust::Rect::new((b[0], b[1]), (b[2], b[3]))
                    .to_polygon()
                    .into()
            };
            let im = g.relate(&probe);
            match side {
                Side::Inside => assert!(im.is_contains_properly(), "{b:?} inside"),
                Side::Outside => assert!(im.is_disjoint(), "{b:?} outside"),
            }
        }
        assert!(decided > 10_000, "{decided}");
    }

    #[test]
    fn edges_on_cell_borders_mark_their_cells() {
        // a square with many vertices along its edges, which lie on cell borders
        let mut ring = Vec::new();
        for i in 0..10 {
            ring.push((i as f64, 0.0));
        }
        for i in 0..10 {
            ring.push((10.0, i as f64));
        }
        for i in 0..10 {
            ring.push((10.0 - i as f64, 10.0));
        }
        for i in 0..10 {
            ring.push((0.0, 10.0 - i as f64));
        }
        ring.push((0.0, 0.0));
        let g = Geometry::Polygon(Polygon::new(LineString::from(ring), vec![]));
        let grid = CellGrid::build(&g, 41).unwrap();
        // points on the edges are never decided
        for p in [[5.0, 0.0], [10.0, 5.0], [0.0, 0.0], [5.0, 10.0]] {
            assert_eq!(grid.side([p[0], p[1], p[0], p[1]]), None, "{p:?}");
        }
        assert_eq!(grid.side([5.0, 5.0, 5.0, 5.0]), Some(Side::Inside));
        assert_eq!(grid.side([11.0, 5.0, 12.0, 6.0]), Some(Side::Outside));
        assert_eq!(grid.side([-3.0, -3.0, -2.0, 20.0]), Some(Side::Outside));
        assert_eq!(grid.side([-3.0, 4.0, 5.0, 5.0]), None);
    }
}
