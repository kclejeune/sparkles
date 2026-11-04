import { describe, expect, test } from 'vitest';
import {
  ancestorsOfRow,
  flattenTree,
  indexOfId,
  keyMove,
  Metrics,
  scrollToRow,
  stickyCap,
  stickyPath,
  type TreeRow,
} from './virtual-tree';

/** A tree given as parent -> children, named by id. */
function treeOf(kids: Record<string, string[]>) {
  return {
    children: (id: string) => kids[id] ?? [],
    name: (id: string) => id,
  };
}

const base = {
  open: () => true,
};

describe('flattenTree', () => {
  test('lists the open nodes depth first, each level in name order', () => {
    const t = treeOf({ a: ['a2', 'a1'], b: ['b1'] });
    const rows = flattenTree({ roots: ['b', 'a'], ...t, ...base });
    expect(rows.map((r) => (r.kind === 'node' ? r.id : 'more'))).toEqual([
      'a',
      'a1',
      'a2',
      'b',
      'b1',
    ]);
    const a1 = rows[1];
    expect(a1).toMatchObject({ depth: 1, parent: 0, pos: 1, size: 2 });
    expect(rows[2]).toMatchObject({ depth: 1, parent: 0, pos: 2, size: 2 });
  });

  test('does not list the children of a closed node', () => {
    const t = treeOf({ a: ['a1'] });
    const rows = flattenTree({ roots: ['a'], ...t, open: () => false });
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ hasKids: true, open: false, kids: 1 });
  });

  test('a node with unread children has a twisty and shows none', () => {
    const t = treeOf({});
    const rows = flattenTree({ roots: ['a'], ...t, open: () => true, more: () => true });
    expect(rows[0]).toMatchObject({ hasKids: true, kids: 0 });
    expect(rows).toHaveLength(1);
  });

  test('a polyhierarchy lists a node under each parent with its own key', () => {
    const t = treeOf({ a: ['x'], b: ['x'] });
    const rows = flattenTree({ roots: ['a', 'b'], ...t, ...base });
    const xs = rows.filter((r) => r.kind === 'node' && r.id === 'x');
    expect(xs).toHaveLength(2);
    expect(new Set(rows.map((r) => r.key)).size).toBe(rows.length);
  });

  test('a cycle does not list a node below itself', () => {
    const t = treeOf({ a: ['b'], b: ['a', 'c'] });
    const rows = flattenTree({ roots: ['a'], ...t, ...base });
    expect(rows.map((r) => (r.kind === 'node' ? r.id : ''))).toEqual(['a', 'b', 'c']);
  });

  test('a filter keeps only its nodes and opens them all', () => {
    const t = treeOf({ a: ['a1', 'a2'], a1: ['a11'] });
    const keep = new Set(['a', 'a1', 'a11']);
    const rows = flattenTree({ roots: ['a', 'z'], ...t, open: () => false, keep });
    expect(rows.map((r) => (r.kind === 'node' ? r.id : ''))).toEqual(['a', 'a1', 'a11']);
  });

  test('a clipped level ends in a more-row, for the roots and for a node', () => {
    const t = treeOf({ a: ['a1'] });
    const clipped = (p: string | null) =>
      p === null ? { shown: 1, total: 5 } : p === 'a' ? { shown: 1, total: 3 } : null;
    const rows = flattenTree({ roots: ['a'], ...t, ...base, clipped });
    expect(rows.map((r) => r.kind)).toEqual(['node', 'node', 'more', 'more']);
    expect(rows[2]).toMatchObject({
      kind: 'more',
      of: 'a',
      shown: 1,
      total: 3,
      depth: 1,
      parent: 0,
    });
    expect(rows[3]).toMatchObject({ kind: 'more', of: null, shown: 1, total: 5, depth: 0 });
    // a level that holds all its children has no more-row
    const full = flattenTree({
      roots: ['a'],
      ...t,
      ...base,
      clipped: () => ({ shown: 3, total: 3 }),
    });
    expect(full.every((r) => r.kind === 'node')).toBe(true);
  });

  test('flattens 100,000 children of one node', () => {
    const ids = Array.from({ length: 100_000 }, (_, i) => `c${i}`);
    const t = treeOf({ root: ids });
    const t0 = performance.now();
    const rows = flattenTree({ roots: ['root'], ...t, ...base });
    expect(rows).toHaveLength(100_001);
    // a generous bound: the point is that it is linear-ish, not that it is fast here
    expect(performance.now() - t0).toBeLessThan(5000);
  });
});

describe('Metrics', () => {
  test('rows of one height sit at multiples of it', () => {
    const m = new Metrics(100, 26);
    expect(m.top(0)).toBe(0);
    expect(m.top(10)).toBe(260);
    expect(m.height).toBe(2600);
    expect(m.at(0)).toBe(0);
    expect(m.at(259)).toBe(9);
    expect(m.at(260)).toBe(10);
    expect(m.at(1e9)).toBe(99);
    expect(m.at(-5)).toBe(0);
  });

  test('a taller row pushes the ones below it down', () => {
    const m = new Metrics(100, 26, [{ at: 3, add: 40 }]);
    expect(m.top(3)).toBe(78);
    expect(m.top(4)).toBe(78 + 66);
    expect(m.height).toBe(2600 + 40);
    expect(m.at(78 + 65)).toBe(3);
    expect(m.at(78 + 66)).toBe(4);
  });

  test('ignores a taller row outside the rows', () => {
    const m = new Metrics(5, 10, [{ at: 9, add: 50 }]);
    expect(m.height).toBe(50);
  });

  test('a window holds the rows in view and an overscan on each side', () => {
    const m = new Metrics(10_000, 26);
    // 5,200 px down, a 520 px view: rows 200 to 220
    expect(m.window(5200, 520, 0)).toEqual({ start: 200, end: 221 });
    expect(m.window(5200, 520, 5)).toEqual({ start: 195, end: 226 });
    expect(m.window(0, 520, 5).start).toBe(0);
    expect(m.window(1e9, 520, 5).end).toBe(10_000);
    expect(new Metrics(0, 26).window(0, 520)).toEqual({ start: 0, end: 0 });
  });

  test('the window of a 100,000-row tree is a screenful', () => {
    const m = new Metrics(100_000, 26);
    const w = m.window(1_300_000, 700, 8);
    expect(w.end - w.start).toBeLessThan(60);
  });
});

describe('scrollToRow', () => {
  const m = new Metrics(1000, 20);
  test('leaves a row in view alone', () => {
    expect(scrollToRow(m, 12, 200, 400, 0)).toBeNull();
  });
  test('brings a row above the view to the top, under the pinned rows', () => {
    expect(scrollToRow(m, 5, 200, 400, 40)).toBe(100 - 40);
    expect(scrollToRow(m, 0, 200, 400, 40)).toBe(0);
  });
  test('brings a row below the view to the bottom', () => {
    expect(scrollToRow(m, 40, 200, 400, 0)).toBe(40 * 20 + 20 - 400);
  });
  test('a row hidden under the pinned rows is moved down', () => {
    // row 10 starts at 200, which the 40 px of pinned rows cover when scrolled to 200
    expect(scrollToRow(m, 10, 200, 400, 40)).toBe(160);
  });
});

describe('stickyPath', () => {
  const t = treeOf({
    r: ['a'],
    a: ['b'],
    b: ['c'],
    c: ['d'],
    d: ['e'],
    e: ['f'],
    f: ['g'],
    g: ['h'],
  });
  const rows = flattenTree({ roots: ['r'], ...t, ...base });
  const idx = (id: string) => indexOfId(rows, id);

  test('the ancestors of a row, from the root', () => {
    expect(ancestorsOfRow(rows, idx('c')).map((i) => (rows[i] as { id: string }).id)).toEqual([
      'r',
      'a',
      'b',
    ]);
    expect(ancestorsOfRow(rows, 0)).toEqual([]);
  });

  test('shows the nearest few and counts the rest', () => {
    const p = stickyPath(rows, idx('h'), 3);
    expect(p.shown.map((i) => (rows[i] as { id: string }).id)).toEqual(['e', 'f', 'g']);
    expect(p.hidden).toBe(5);
  });

  test('never more than six', () => {
    expect(stickyPath(rows, idx('h'), 50).shown).toHaveLength(6);
  });

  test('a short view keeps most of its rows', () => {
    expect(stickyCap(200, 26)).toBe(2);
    expect(stickyCap(100, 26)).toBe(1);
    expect(stickyCap(2000, 26)).toBe(6);
  });
});

describe('keyMove', () => {
  const t = treeOf({ a: ['a1', 'a2'], a1: ['x'] });
  const closed: TreeRow[] = flattenTree({
    roots: ['a', 'b'],
    ...t,
    open: (id) => id === 'a',
  });
  // a, a1 (closed, has x), a2, b
  test('arrows and Home and End move the cursor within the rows', () => {
    expect(keyMove(closed, 0, 'ArrowDown', 5)).toEqual({ to: 1 });
    expect(keyMove(closed, 3, 'ArrowDown', 5)).toEqual({ to: 3 });
    expect(keyMove(closed, 0, 'ArrowUp', 5)).toEqual({ to: 0 });
    expect(keyMove(closed, 2, 'Home', 5)).toEqual({ to: 0 });
    expect(keyMove(closed, 0, 'End', 5)).toEqual({ to: 3 });
    expect(keyMove(closed, 0, 'PageDown', 2)).toEqual({ to: 2 });
    expect(keyMove(closed, 3, 'PageUp', 2)).toEqual({ to: 1 });
  });

  test('Right opens a closed node, then enters it', () => {
    expect(keyMove(closed, 1, 'ArrowRight', 5)).toEqual({ toggle: 'a1' });
    expect(keyMove(closed, 0, 'ArrowRight', 5)).toEqual({ to: 1 });
    expect(keyMove(closed, 3, 'ArrowRight', 5)).toBeNull();
  });

  test('Left closes an open node, else goes to the parent', () => {
    expect(keyMove(closed, 0, 'ArrowLeft', 5)).toEqual({ toggle: 'a' });
    expect(keyMove(closed, 2, 'ArrowLeft', 5)).toEqual({ to: 0 });
    expect(keyMove(closed, 3, 'ArrowLeft', 5)).toBeNull();
  });

  test('Enter and Space select a node and read more on a more-row', () => {
    expect(keyMove(closed, 2, 'Enter', 5)).toEqual({ select: 'a2' });
    expect(keyMove(closed, 2, ' ', 5)).toEqual({ select: 'a2' });
    const withMore = flattenTree({
      roots: ['a'],
      ...t,
      ...base,
      clipped: (p) => (p === 'a' ? { shown: 2, total: 9 } : null),
    });
    const at = withMore.findIndex((r) => r.kind === 'more');
    expect(keyMove(withMore, at, 'Enter', 5)).toEqual({ more: 'a' });
  });

  test('other keys and an empty tree do nothing', () => {
    expect(keyMove(closed, 0, 'x', 5)).toBeNull();
    expect(keyMove([], 0, 'ArrowDown', 5)).toBeNull();
  });
});
