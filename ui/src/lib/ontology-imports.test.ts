import { describe, expect, it } from 'vitest';
import { closureCounts, closureTree, partKey, type ImportEdge } from './ontology-imports';

const e = (from: string, iri: string, graphs: string[], cycle = false): ImportEdge => ({
  from,
  iri,
  graphs,
  via: graphs.length ? 'graph' : null,
  cycle,
});

describe('closureTree', () => {
  it('draws each graph once and shows a cycle where it closes', () => {
    const edges = [
      e('urn:a', 'urn:b', ['urn:b'], true),
      e('urn:a', 'urn:c', ['urn:c']),
      e('urn:a', 'urn:missing', []),
      e('urn:b', 'urn:a', ['urn:a'], true),
      e('urn:c', 'urn:b', ['urn:b'], true),
    ];
    const t = closureTree('urn:a', edges);
    expect(t.map((n) => n.edge.iri)).toEqual(['urn:b', 'urn:c', 'urn:missing']);
    const [b, c, missing] = t;
    expect(b.children.map((n) => [n.edge.iri, n.repeated, n.edge.cycle])).toEqual([
      ['urn:a', true, true],
    ]);
    expect(c.children.map((n) => [n.edge.iri, n.repeated])).toEqual([['urn:b', true]]);
    expect(missing.children).toEqual([]);
    expect(missing.repeated).toBe(false);
    expect(closureCounts(edges)).toEqual({ imports: 4, unresolved: 1, cycles: 3 });
  });

  it('expands a resource import into each of its graphs', () => {
    const t = closureTree('urn:a', [
      e('urn:a', 'http://v/ns', ['urn:v1', 'urn:v2']),
      e('urn:v2', 'urn:x', ['urn:x']),
    ]);
    expect(t[0].children.map((n) => n.edge.iri)).toEqual(['urn:x']);
  });
});

describe('partKey', () => {
  it('keys a folder file by its path below the folder', () => {
    expect(partKey({ name: 'b.ttl', webkitRelativePath: 'onto/sub/b.ttl' })).toBe('sub/b.ttl');
    expect(partKey({ name: 'a.owl', webkitRelativePath: '' })).toBe('a.owl');
    expect(partKey({ name: 'a.owl' })).toBe('a.owl');
  });
});
