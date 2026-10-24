import { describe, expect, it } from 'vitest';
import type { GEdge, GNode } from '$components/GraphView.svelte';
import type { Term } from './api';
import { mergePaths, placeBetween } from './path-overlay';
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

  it('leaves the graph as drawn when there are no paths', () => {
    const m = mergePaths(nodes, edges, { nodes: [], edges: [] }, String, String);
    expect(m.nodes).toEqual(nodes);
    expect(m.edges).toEqual(edges);
    expect(m.members.size).toBe(0);
  });
});

describe('placeBetween', () => {
  const es: GEdge[] = [
    { id: '1', source: 'a', target: 'n', label: '', paths: [1] },
    { id: '2', source: 'n', target: 'b', label: '', paths: [1] },
    { id: '3', source: 'n', target: 'z', label: '' },
  ];
  const at = (id: string) => ({ a: { x: 0, y: 0 }, b: { x: 10, y: 20 }, z: { x: 99, y: 99 } })[id];
  it('places a node between its neighbours on the paths', () => {
    expect(placeBetween('n', es, at, { x: -1, y: -1 })).toEqual({ x: 8, y: 13 });
  });
  it('falls back when no neighbour has a place', () => {
    expect(placeBetween('q', es, at, { x: -1, y: -1 })).toEqual({ x: -1, y: -1 });
  });
});
