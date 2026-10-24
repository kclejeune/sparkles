import { describe, expect, it } from 'vitest';
import type { GEdge, GNode } from '$components/GraphView.svelte';
import type { Term } from './api';
import { mergePaths, placeAdded } from './path-overlay';
import { edgeId } from './paths';
import { termKey } from './rdf';

const EX = 'http://example.org/';
const iri = (l: string): Term => ({ type: 'uri', value: EX + l });
const lit = (v: string): Term => ({ type: 'literal', value: v });
const P = `${EX}p`;

// a result graph as the query page draws it: IRI nodes by their term key, a literal per edge
const nodes: GNode[] = [
  { id: termKey(iri('a')), label: 'a', kind: 'iri', term: iri('a') },
  { id: termKey(iri('b')), label: 'b', kind: 'iri', term: iri('b') },
  { id: 'lit:x', label: '"x"', kind: 'literal', term: lit('x') },
];
const edges: GEdge[] = [
  {
    id: 'e0',
    source: termKey(iri('a')),
    target: termKey(iri('b')),
    label: 'p',
    triple: [iri('a'), { type: 'uri', value: P }, iri('b')],
  },
  {
    id: 'e1',
    source: termKey(iri('b')),
    target: 'lit:x',
    label: 'p',
    triple: [iri('b'), { type: 'uri', value: P }, lit('x')],
  },
];
const oEdge = (s: Term, o: Term, paths: number[], order: number) => ({
  id: edgeId(s, P, o),
  s,
  p: P,
  o,
  paths,
  order,
});
const oNode = (t: Term, paths: number[], order: number) => ({
  key: termKey(t),
  term: t,
  paths,
  order,
});

describe('mergePaths', () => {
  it('marks the drawn edges a path uses, through literal nodes too, and adds what is missing', () => {
    const m = mergePaths(
      nodes,
      edges,
      {
        nodes: [
          oNode(iri('a'), [1, 2], 0),
          oNode(iri('b'), [1], 1),
          oNode(lit('x'), [1], 2),
          oNode(iri('c'), [2], 1),
        ],
        edges: [
          oEdge(iri('a'), iri('b'), [1], 0),
          oEdge(iri('b'), lit('x'), [1], 1),
          oEdge(iri('a'), iri('c'), [2], 2),
        ],
      },
      (t) => `L(${t.value})`,
      (p) => `E(${p})`,
    );
    expect(m.added).toEqual({ nodes: 1, edges: 1 });
    expect(m.edges.find((e) => e.id === 'e0')?.paths).toEqual([1]);
    // the literal of the result is found through its edge, not added again
    expect(m.nodes.find((n) => n.id === 'lit:x')?.paths).toEqual([1]);
    const c = m.nodes.find((n) => n.id === termKey(iri('c')));
    expect(c).toMatchObject({ label: `L(${EX}c)`, kind: 'iri', paths: [2] });
    const added = m.edges.find((e) => e.id.startsWith('path:'));
    expect(added).toMatchObject({
      source: termKey(iri('a')),
      target: termKey(iri('c')),
      label: `E(${P})`,
      paths: [2],
    });
    expect([...m.members.get(1)!.edges].sort()).toEqual(['e0', 'e1']);
    expect([...m.members.get(2)!.nodes].sort()).toEqual(
      [termKey(iri('a')), termKey(iri('c'))].sort(),
    );
    // the input is left as it was
    expect(nodes.every((n) => n.paths === undefined)).toBe(true);
  });

  it('marks each literal node of the result by the triple that reaches it', () => {
    // the result draws a node per literal object: "x" of a and "x" of b are two nodes
    const ns: GNode[] = [
      { id: termKey(iri('a')), label: 'a', kind: 'iri', term: iri('a') },
      { id: termKey(iri('b')), label: 'b', kind: 'iri', term: iri('b') },
      { id: 'lit:a', label: '"x"', kind: 'literal', term: lit('x') },
      { id: 'lit:b', label: '"x"', kind: 'literal', term: lit('x') },
    ];
    const es: GEdge[] = [
      {
        id: 'e0',
        source: termKey(iri('a')),
        target: 'lit:a',
        label: 'p',
        triple: [iri('a'), { type: 'uri', value: P }, lit('x')],
      },
      {
        id: 'e1',
        source: termKey(iri('b')),
        target: 'lit:b',
        label: 'p',
        triple: [iri('b'), { type: 'uri', value: P }, lit('x')],
      },
    ];
    const m = mergePaths(
      ns,
      es,
      {
        nodes: [oNode(iri('a'), [1], 0), oNode(iri('b'), [2], 0), oNode(lit('x'), [1, 2], 1)],
        edges: [oEdge(iri('a'), lit('x'), [1], 0), oEdge(iri('b'), lit('x'), [2], 1)],
      },
      String,
      String,
    );
    expect(m.nodes.find((n) => n.id === 'lit:a')?.paths).toEqual([1]);
    expect(m.nodes.find((n) => n.id === 'lit:b')?.paths).toEqual([2]);
    expect([...m.members.get(1)!.nodes].sort()).toEqual(['lit:a', termKey(iri('a'))].sort());
    expect(m.added).toEqual({ nodes: 0, edges: 0 });
  });

  it('gives a literal that a path adds a node of its own per triple', () => {
    const m = mergePaths(
      nodes.slice(0, 2),
      edges.slice(0, 1),
      {
        nodes: [oNode(iri('a'), [1, 2], 0), oNode(iri('b'), [2], 0), oNode(lit('y'), [1, 2], 1)],
        edges: [oEdge(iri('a'), lit('y'), [1], 0), oEdge(iri('b'), lit('y'), [2], 1)],
      },
      String,
      String,
    );
    const added = m.nodes.filter((n) => n.kind === 'literal');
    expect(added.map((n) => n.paths)).toEqual([[1], [2]]);
    expect(new Set(added.map((n) => n.id)).size).toBe(2);
    expect(m.added).toEqual({ nodes: 2, edges: 2 });
  });

  it('leaves the graph as drawn when there are no paths', () => {
    const m = mergePaths(nodes, edges, { nodes: [], edges: [] }, String, String);
    expect(m.nodes).toEqual(nodes);
    expect(m.edges).toEqual(edges);
    expect(m.members.size).toBe(0);
  });
});

describe('placeAdded', () => {
  const at = (id: string) => ({ a: { x: 0, y: 0 }, b: { x: 1000, y: 0 }, z: { x: 99, y: 99 } })[id];
  const far = { x: -1, y: -1 };
  it('spreads a chain of new nodes evenly between the drawn ends of a path', () => {
    const p = placeAdded(
      [
        { id: 'n1', fallback: far },
        { id: 'n2', fallback: far },
      ],
      [
        { source: 'a', target: 'n1', paths: [1] },
        { source: 'n1', target: 'n2', paths: [1] },
        { source: 'n2', target: 'b', paths: [1] },
        { source: 'n1', target: 'z' },
      ],
      at,
    );
    expect(Math.round(p.get('n1')!.x)).toBe(336);
    expect(Math.round(p.get('n2')!.x)).toBe(670);
  });
  it('keeps a path between two nodes the result lacks apart, at their fallbacks', () => {
    const p = placeAdded(
      [
        { id: 's', fallback: { x: 0, y: 0 } },
        { id: 't', fallback: { x: 100, y: 50 } },
      ],
      [{ source: 's', target: 't', paths: [1] }],
      at,
    );
    expect(p.get('s')).toEqual({ x: 0, y: 0 });
    expect(p.get('t')).toEqual({ x: 100, y: 50 });
  });
  it('fans a group with one drawn neighbour out from it, one gap per edge', () => {
    const p = placeAdded(
      [
        { id: 'n1', fallback: { x: 500, y: 0 } },
        { id: 'n2', fallback: { x: 600, y: 0 } },
        { id: 'm', fallback: { x: 500, y: 10 } },
      ],
      [
        { source: 'a', target: 'n1', paths: [1] },
        { source: 'n1', target: 'n2', paths: [1] },
        { source: 'a', target: 'm', paths: [2] },
      ],
      at,
      20,
    );
    const pts = ['n1', 'n2', 'm'].map((k) => p.get(k)!);
    // n1 and m are one edge from a, n2 two, and no two of them share a place
    expect(Math.round(Math.hypot(pts[1].x, pts[1].y))).toBe(40);
    expect(new Set(pts.map((q) => `${Math.round(q.x)},${Math.round(q.y)}`)).size).toBe(3);
    for (const q of pts) expect(Math.hypot(q.x, q.y)).toBeGreaterThan(10);
  });
  it('keeps the fallback of a node with no neighbour on a path, without reading other edges', () => {
    const edges = Array.from({ length: 1000 }, (_, i) => ({ source: 'q', target: `x${i}` }));
    expect(placeAdded([{ id: 'q', fallback: far }], edges, at).get('q')).toEqual(far);
  });
});
