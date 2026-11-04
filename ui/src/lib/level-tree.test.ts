import { describe, expect, it } from 'vitest';
import type { Hierarchy, TreeNode } from './browse';
import { filteredTree, LevelTree } from './level-tree.svelte';

const node = (iri: string, children = 0): TreeNode => ({
  iri,
  label: iri.toUpperCase(),
  labelLang: null,
  shortForm: null,
  kinds: ['class'],
  deprecated: false,
  children,
  parents: 0,
});

const answer = (h: Partial<Hierarchy>): Hierarchy => ({
  dataset: 'd',
  commit: 1,
  kind: 'class',
  node: null,
  children: [],
  total: h.children?.length ?? 0,
  offset: 0,
  limit: 1000,
  next: null,
  order: 'label',
  ...h,
});

// a: b, c; b: d
const KIDS: Record<string, TreeNode[]> = {
  '': [node('a', 2)],
  a: [node('b', 1), node('c')],
  b: [node('d')],
};
const PARENTS: Record<string, string> = { b: 'a', c: 'a', d: 'b' };

function reader(log: string[]) {
  return async (q: { iri?: string; ancestors?: boolean }) => {
    log.push(`${q.iri ?? ''}${q.ancestors ? '^' : ''}`);
    if (q.ancestors) {
      const edges: [string, string][] = [];
      const nodes: TreeNode[] = [];
      for (let n = q.iri!; PARENTS[n]; n = PARENTS[n]) {
        edges.push([n, PARENTS[n]]);
        nodes.push(node(PARENTS[n], 1));
      }
      return answer({ node: node(q.iri!), ancestors: { nodes, edges, truncated: false } });
    }
    return answer({ node: q.iri ? node(q.iri) : null, children: KIDS[q.iri ?? ''] ?? [] });
  };
}

describe('LevelTree', () => {
  it('reads the roots, then a level once when its node opens', async () => {
    const log: string[] = [];
    const names: string[] = [];
    const t = new LevelTree(reader(log), (ns) => names.push(...ns.map((n) => n.iri)));
    await t.load(null);
    expect(t.roots()).toEqual(['a']);
    expect(t.more('a')).toBe(true);
    await Promise.all([t.load('a'), t.load('a')]);
    expect(t.children('a')).toEqual(['b', 'c']);
    expect(t.more('a')).toBe(false);
    expect(t.more('c')).toBe(false);
    expect(log).toEqual(['', 'a']);
    expect(names).toContain('b');
  });

  it('reveals the path to a deep node and answers the ancestors to open', async () => {
    const log: string[] = [];
    const t = new LevelTree(reader(log));
    const anc = await t.reveal('d');
    expect(new Set(anc)).toEqual(new Set(['a', 'b']));
    expect(t.children('a')).toEqual(['b', 'c']);
    expect(t.children('b')).toEqual(['d']);
  });

  it('forgets its levels on reset and reads the open ones again', async () => {
    const log: string[] = [];
    const t = new LevelTree(reader(log));
    await t.load(null);
    await t.load('a');
    t.reset(['a']);
    expect(t.roots()).toEqual([]);
    await t.load(null);
    await t.load('a');
    expect(t.children('a')).toEqual(['b', 'c']);
    expect(log).toEqual(['', 'a', '', 'a']);
  });
});

describe('filteredTree', () => {
  it('joins the hits and their ancestors into one tree', () => {
    const tree = filteredTree([
      answer({
        node: node('d'),
        ancestors: {
          nodes: [node('b'), node('a')],
          edges: [
            ['d', 'b'],
            ['b', 'a'],
          ],
          truncated: false,
        },
      }),
      answer({
        node: node('c'),
        ancestors: { nodes: [node('a')], edges: [['c', 'a']], truncated: false },
      }),
    ]);
    expect(tree.roots).toEqual(['a']);
    expect(tree.children.get('a')?.sort()).toEqual(['b', 'c']);
    expect(tree.children.get('b')).toEqual(['d']);
  });
});

describe('a clipped level', () => {
  it('reads the rest a page at a time', async () => {
    const all = Array.from({ length: 25 }, (_, i) => `urn:c${String(i).padStart(2, '0')}`);
    const node = (iri: string) =>
      ({
        iri,
        label: iri,
        labelLang: null,
        shortForm: null,
        kinds: ['class'],
        deprecated: false,
        children: 0,
        parents: 1,
      }) as never;
    const t = new LevelTree(async (q) => {
      const off = q.offset ?? 0;
      const page = q.iri ? all.slice(off, off + 10) : ['urn:p'];
      return {
        dataset: 'd',
        commit: 1,
        kind: 'class',
        node: null,
        children: page.map(node),
        total: q.iri ? all.length : 1,
        offset: off,
        limit: 10,
        next: null,
        order: 'label',
      };
    });
    await t.load('urn:p');
    expect(t.clippedOf('urn:p')).toEqual({ shown: 10, total: 25 });
    await t.loadMore('urn:p');
    await t.loadMore('urn:p');
    expect(t.children('urn:p')).toEqual(all);
    expect(t.clippedOf('urn:p')).toBeNull();
  });
});
