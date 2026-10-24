import { describe, expect, it } from 'vitest';
import type { QueryOptions, Term } from './api';
import { Lenses, resultState, type LensInput } from './lenses.svelte';
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

  it('stops a shared inferred-edge read when the canvas goes (re-review 2)', async () => {
    const signals: AbortSignal[] = [];
    const l = new Lenses();
    l.delay = 0;
    l.select = (async (_ds: string, _q: string, opts: QueryOptions) => {
      signals.push(opts.signal!);
      await new Promise((r) => setTimeout(r, 200));
      return [];
    }) as SelectFn;
    const edges: LensEdge[] = Array.from({ length: 1000 }, (_, i) => ({
      id: `e${i}`,
      source: 'a',
      target: `n${i}`,
      triple: [iri(`${EX}a`), iri(`${EX}p`), iri(`${EX}n${i}`)],
    }));
    l.setInput(input({ edges, reasoning: true }));
    l.set('badge', { lens: 'inferred' });
    await settle();
    expect(signals.length).toBeGreaterThan(0);
    l.destroy();
    expect(signals.every((s) => s?.aborted)).toBe(true);
  });
});

describe('Lenses after a merge review', () => {
  it('drops the values of an earlier state when the read of a new one fails', async () => {
    let fail = false;
    const { l } = lenses((s) => {
      if (fail && s.includes('age')) throw new Error('read failed');
      return s.includes('age') ? [{ n: iri(`${EX}a`), v: lit('41') }] : [];
    });
    l.setInput(input({ at: 'commit:1' }));
    l.set('size', { lens: 'property', property: `${EX}age` });
    await settle();
    expect(l.results.size?.values.get(`<${EX}a>`)).toBe(41);
    fail = true;
    l.setInput(input({ at: 'commit:2' }));
    await settle();
    expect(l.status.size?.error).toBeTruthy();
    expect(l.results.size ?? null).toBeNull();
    l.destroy();
  });

  it('keeps the properties of the latest drawing when an earlier read ends last', async () => {
    const waits: (() => void)[] = [];
    const { l } = lenses(() => []);
    l.select = (async (ds: string) => {
      // the first dataset's read ends after the second's
      if (ds === 'a') await new Promise<void>((r) => waits.push(r));
      return [{ p: iri(`${EX}${ds}prop`), c: lit('1'), num: lit('1') }];
    }) as SelectFn;
    l.setInput(input({ ds: 'a' }));
    const first = l.loadProperties();
    l.setInput(input({ ds: 'b' }));
    await l.loadProperties();
    expect(l.properties.length).toBeGreaterThan(0);
    const shown = JSON.stringify(l.properties);
    expect(shown).toContain('bprop');
    waits.forEach((r) => r());
    await first;
    expect(JSON.stringify(l.properties)).toBe(shown);
    l.destroy();
  });
});

describe('resultState', () => {
  it('reads a result at its commit, whether or not an At selector named it (re-review 1)', () => {
    expect(resultState({ meta: { commit: 7 } })).toBe('commit:7');
    expect(resultState({ at: { commit: 3 }, meta: { commit: 7 } })).toBe('commit:3');
    expect(resultState({ meta: {} })).toBeUndefined();
  });
});

describe('the validation lens (U07 §11.2, item 120)', () => {
  const badge = (count: number, worst: 'violation' | 'warning' | 'info' = 'violation') => ({
    count,
    worst,
    words: `${count} ${worst}${count === 1 ? '' : 's'}`,
  });
  it('is offered only where the overlay may validate, and says why not', () => {
    const { l } = lenses(() => []);
    l.setInput(input({}));
    expect(l.validation).toBe(false);
    l.setValidation(false, 'Validation needs access to every graph.');
    expect(l.validationNote).toBe('Validation needs access to every graph.');
    l.setValidation(true);
    expect(l.validation).toBe(true);
    expect(l.validationNote).toBeNull();
    l.destroy();
  });

  it('is a default that leaves a chosen badge lens alone, and ends when it is not offered', async () => {
    const { l } = lenses(() => []);
    const chosen: string[] = [];
    l.onchoose = (ch, c) => chosen.push(`${ch}:${c.lens}`);
    l.setInput(input({ nodes: [node('a'), node('b')] }));
    l.setValidation(true);
    l.suggest('badge', { lens: 'validation' });
    expect(l.choice.badge.lens).toBe('validation');
    // a default is not the person's choice
    expect(chosen).toEqual([]);
    l.setValidationBadges(new Map([[`<${EX}a>`, badge(2)]]));
    await settle();
    const r = l.results.badge!;
    expect(r.lens).toBe('validation');
    expect(r.text.get(`<${EX}a>`)).toBe('✕ 2 violations');
    expect(r.legend.map((e) => e.ids)).toEqual([[`<${EX}a>`]]);
    expect(l.columns.find((c) => c.channel === 'badge')?.title).toBe('validation results');
    // the overlay draws these badges, so the canvas encoding has none
    expect(l.encoding.badge).toBe(false);
    expect(l.encoding.nodes.get(`<${EX}a>`)?.badge).toBeUndefined();
    // the person turns it off, and a default no longer applies
    l.set('badge', { lens: 'none' });
    expect(chosen).toEqual(['badge:none']);
    l.suggest('badge', { lens: 'validation' });
    expect(l.choice.badge.lens).toBe('none');
    l.set('badge', { lens: 'validation' });
    l.setValidation(false, 'Validation needs access to every graph.');
    expect(l.choice.badge.lens).toBe('none');
    l.destroy();
  });

  it('a default of one dataset is not carried to the next', () => {
    const { l } = lenses(() => []);
    l.setInput(input({ ds: 'one' }));
    l.setValidation(true);
    l.suggest('badge', { lens: 'validation' });
    expect(l.choice.badge.lens).toBe('validation');
    l.setInput(input({ ds: 'two' }));
    expect(l.choice.badge.lens).toBe('none');
    l.destroy();
  });
});

describe('the validation lens after review (item 120)', () => {
  it('an explicit choice comes back when validation is offered again', () => {
    const { l } = lenses(() => []);
    l.setInput(input({ ds: 'a' }));
    l.setValidation(true);
    l.set('badge', { lens: 'validation' });
    // a restricted dataset does not offer it
    l.setInput(input({ ds: 'b' }));
    l.setValidation(false, 'Validation needs access to every graph.');
    expect(l.choice.badge.lens).toBe('none');
    // back on the first dataset, the page restores the remembered choice
    l.setInput(input({ ds: 'a' }));
    l.setValidation(true);
    l.restore('badge', { lens: 'validation' });
    expect(l.choice.badge.lens).toBe('validation');
    l.destroy();
  });

  it('Shift+3 turns on the remembered badge lens, also with reasoning', () => {
    const { l } = lenses(() => []);
    l.setLast('badge', { lens: 'validation' });
    l.setInput(input({ reasoning: true }));
    l.setValidation(true);
    l.toggle('badge');
    expect(l.choice.badge.lens).toBe('validation');
    l.destroy();
  });
});

describe('the validation lens after the second review (item 120)', () => {
  it('turning off a default lens names it for Shift+3 after a reload', () => {
    const { l } = lenses(() => []);
    const remembered: string[] = [];
    l.onchoose = (_ch, _c, remember) => remembered.push(remember.lens);
    l.setInput(input({ reasoning: true }));
    l.setValidation(true);
    l.suggest('badge', { lens: 'validation' });
    l.set('badge', { lens: 'none' });
    expect(remembered).toEqual(['validation']);
    l.destroy();
    // the page opens again with what it saved
    const { l: again } = lenses(() => []);
    again.setLast('badge', { lens: remembered[0] as 'validation' });
    again.setInput(input({ reasoning: true }));
    again.setValidation(true);
    again.toggle('badge');
    expect(again.choice.badge.lens).toBe('validation');
    again.destroy();
  });
});
