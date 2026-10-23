import { describe, expect, it } from 'vitest';
import type { QueryOptions, Term } from './api';
import { Lenses, type LensInput } from './lenses.svelte';
import type { LensEdge, LensNode, SelectFn } from './lens-sources';

const EX = 'http://example.org/';
const iri = (v: string): Term => ({ type: 'uri', value: v });
const lit = (v: string): Term => ({ type: 'literal', value: v });
const node = (n: string): LensNode => ({ id: `<${EX}${n}>`, term: iri(EX + n) });

const settle = () => new Promise((r) => setTimeout(r, 30));

function lenses(answer: (sparql: string, opts: QueryOptions) => Record<string, Term>[]) {
  const calls: { sparql: string; opts: QueryOptions }[] = [];
  const l = new Lenses();
  l.delay = 0;
  l.select = (async (_ds: string, sparql: string, opts: QueryOptions) => {
    calls.push({ sparql, opts });
    return answer(sparql, opts);
  }) as SelectFn;
  return { l, calls };
}

const input = (over: Partial<LensInput>): LensInput => ({
  ds: 'ds',
  nodes: [node('a')],
  edges: [],
  reasoning: false,
  label: (id) => id,
  short: (i) => i,
  ...over,
});

describe('Lenses', () => {
  it('reads a property again when the graph is redrawn from new reads (review 2)', async () => {
    let age = '41';
    const { l, calls } = lenses((s) =>
      s.includes('age') ? [{ n: iri(`${EX}a`), v: lit(age) }] : [],
    );
    l.setInput(input({ epoch: 1 }));
    l.set('size', { lens: 'property', property: `${EX}age` });
    await settle();
    expect(l.results.size?.values.get(`<${EX}a>`)).toBe(41);
    const reads = calls.length;
    // the same drawing reads nothing again
    l.setInput(input({ epoch: 1 }));
    await settle();
    expect(calls.length).toBe(reads);
    // the head moved and the page drew the graph again
    age = '42';
    l.setInput(input({ epoch: 2 }));
    await settle();
    expect(l.results.size?.values.get(`<${EX}a>`)).toBe(42);
    l.destroy();
  });

  it('clears the inferred lenses when the dataset has no reasoning (review 3)', async () => {
    const e: LensEdge = {
      id: 'e1',
      source: `<${EX}a>`,
      target: `<${EX}b>`,
      triple: [iri(`${EX}a`), iri(`${EX}p`), iri(`${EX}b`)],
    };
    const { l } = lenses(() => []);
    l.setInput(input({ edges: [e], reasoning: true }));
    await settle();
    expect(l.choice.edge.lens).toBe('inferred');
    expect(l.results.edge?.codes.has('e1')).toBe(true);
    l.setInput(input({ ds: 'other', edges: [e], reasoning: false }));
    await settle();
    expect(l.choice.edge.lens).toBe('none');
    expect(l.results.edge ?? null).toBeNull();
    expect(l.encoding.inferred.size).toBe(0);
    l.destroy();
  });

  it('lists the properties of each graph, even of the same size (review 5)', async () => {
    const { l } = lenses((s) =>
      s.includes(`<${EX}a>`)
        ? [{ p: iri(`${EX}age`), c: lit('1'), num: lit('1') }]
        : [{ p: iri(`${EX}cost`), c: lit('1'), num: lit('1') }],
    );
    l.setInput(input({ nodes: [node('a')] }));
    await l.loadProperties();
    expect(l.properties.map((p) => p.iri)).toEqual([`${EX}age`]);
    l.setInput(input({ nodes: [node('b')] }));
    await l.loadProperties();
    expect(l.properties.map((p) => p.iri)).toEqual([`${EX}cost`]);
    l.destroy();
  });
});
