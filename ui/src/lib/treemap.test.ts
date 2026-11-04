import { describe, expect, it } from 'vitest';
import { layout, squarify, type Rect, type TreemapNode } from './treemap';

const area = (r: Rect) => r.w * r.h;
const near = (a: number, b: number, eps = 1e-6) =>
  Math.abs(a - b) <= eps * Math.max(1, Math.abs(b));

describe('squarify', () => {
  const box: Rect = { x: 0, y: 0, w: 600, h: 400 };

  it('fills the box with areas in proportion to the weights', () => {
    const weights = [6, 6, 4, 3, 2, 2, 1];
    const rects = squarify(weights, box);
    const total = weights.reduce((a, b) => a + b, 0);
    expect(rects).toHaveLength(weights.length);
    for (const [i, r] of rects.entries())
      expect(near(area(r), (area(box) * weights[i]) / total)).toBe(true);
    expect(
      near(
        rects.reduce((a, r) => a + area(r), 0),
        area(box),
      ),
    ).toBe(true);
  });

  it('keeps every rectangle inside the box and apart from the others', () => {
    const rects = squarify([9, 5, 5, 3, 2, 1, 1, 1], box);
    for (const r of rects) {
      expect(r.x).toBeGreaterThanOrEqual(-1e-9);
      expect(r.y).toBeGreaterThanOrEqual(-1e-9);
      expect(r.x + r.w).toBeLessThanOrEqual(box.w + 1e-9);
      expect(r.y + r.h).toBeLessThanOrEqual(box.h + 1e-9);
    }
    for (let i = 0; i < rects.length; i++)
      for (let j = i + 1; j < rects.length; j++) {
        const a = rects[i];
        const b = rects[j];
        const ox = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x);
        const oy = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
        expect(ox <= 1e-6 || oy <= 1e-6).toBe(true);
      }
  });

  it('makes the rectangles near square', () => {
    const rects = squarify([5, 5, 5, 5, 5, 5], { x: 0, y: 0, w: 300, h: 200 });
    for (const r of rects) expect(Math.max(r.w / r.h, r.h / r.w)).toBeLessThan(2);
  });

  it('draws a weight under 1 and an empty list', () => {
    const rects = squarify([0, 10], box);
    expect(area(rects[0])).toBeGreaterThan(0);
    expect(squarify([], box)).toEqual([]);
    expect(squarify([1, 2], { x: 0, y: 0, w: 0, h: 10 }).every((r) => area(r) === 0)).toBe(true);
  });
});

describe('layout', () => {
  const node = (id: string, weight: number, children: TreemapNode[] = []): TreemapNode => ({
    id,
    weight,
    children,
  });

  it('puts a parent before its children and the children inside the parent', () => {
    const forest = [node('a', 6, [node('a1', 4), node('a2', 2)]), node('b', 2)];
    const boxes = layout(forest, 600, 400);
    expect(boxes.map((b) => b.id)).toEqual(['a', 'a1', 'a2', 'b']);
    const a = boxes.find((b) => b.id === 'a')!;
    expect(a.leaf).toBe(false);
    for (const id of ['a1', 'a2']) {
      const c = boxes.find((b) => b.id === id)!;
      expect(c.parent).toBe('a');
      expect(c.depth).toBe(1);
      expect(c.rect.x).toBeGreaterThanOrEqual(a.rect.x);
      expect(c.rect.y).toBeGreaterThanOrEqual(a.rect.y + 24);
      expect(c.rect.x + c.rect.w).toBeLessThanOrEqual(a.rect.x + a.rect.w + 1e-9);
      expect(c.rect.y + c.rect.h).toBeLessThanOrEqual(a.rect.y + a.rect.h + 1e-9);
    }
  });

  it('shows a parent as a leaf when its children would not fit', () => {
    const forest = [node('a', 1, [node('a1', 1)]), node('b', 400)];
    const boxes = layout(forest, 300, 200);
    expect(boxes.map((b) => b.id)).toEqual(['a', 'b']);
    expect(boxes.find((b) => b.id === 'a')!.leaf).toBe(true);
  });
});
