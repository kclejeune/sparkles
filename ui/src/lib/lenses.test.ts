import { afterEach, describe, expect, it, vi } from 'vitest';
import type { QueryOptions, Task, Term } from './api';
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

describe('algorithm lenses (Phase 2)', () => {
  const people = ['hub', 'a', 'b', 'c'].map(node);
  const settleLong = () => new Promise((r) => setTimeout(r, 80));

  it('runs a drawn-graph algorithm once the canvas is still, over the drawn nodes', async () => {
    const { l, calls } = lenses((s) =>
      s.includes('SERVICE algo:pageRank')
        ? [
            {
              node: iri(`${EX}hub`),
              score: lit('0.4'),
              summary: lit('{"iterations":9,"converged":true}'),
            },
            {
              node: iri(`${EX}a`),
              score: lit('0.2'),
              summary: lit('{"iterations":9,"converged":true}'),
            },
          ]
        : [],
    );
    l.stillDelay = 40;
    l.setInput(input({ nodes: people }));
    l.set('size', { lens: 'pageRank' });
    await settleLong();
    const q = calls.find((c) => c.sparql.includes('SERVICE algo:pageRank'))!.sparql;
    expect(q).toContain(`VALUES ?s { <${EX}hub> <${EX}a> <${EX}b> <${EX}c> }`);
    const r = l.results.size!;
    expect(r.values.get(`<${EX}hub>`)).toBe(0.4);
    expect(r.values.get(`<${EX}b>`)).toBeNull();
    expect(r.summary).toBe('9 iterations, converged');
    expect(r.name).toBe('PageRank in the drawn graph');
    // a redrawn graph waits for the canvas to be still again
    const n = calls.length;
    l.setInput(input({ nodes: [...people, node('d')] }));
    await settle();
    expect(calls.length).toBe(n);
    await settleLong();
    expect(calls.length).toBe(n + 1);
    l.destroy();
  });

  it('keeps community colours across runs with another seed (A4)', async () => {
    let seed42 = true;
    const { l } = lenses((s) => {
      if (!s.includes('SERVICE algo:leiden')) return [];
      seed42 = s.includes('algo:seed 42');
      const of = seed42
        ? { hub: '1', a: '1', b: '2', c: '2' }
        : { hub: '8', a: '8', b: '5', c: '5' };
      return Object.entries(of).map(([n, c]) => ({
        node: iri(EX + n),
        community: lit(c),
        level: lit('0'),
      }));
    });
    l.stillDelay = 0;
    l.setInput(input({ nodes: people }));
    l.set('colour', { lens: 'leiden' });
    await settleLong();
    const before = new Map(l.results.colour!.codes);
    l.set('colour', { lens: 'leiden', params: { seed: 43 } });
    await settleLong();
    expect(seed42).toBe(false);
    expect(new Map(l.results.colour!.codes)).toEqual(before);
    expect(l.results.colour!.legend[0].text).toMatch(/^community \d, 2 nodes$/);
    l.destroy();
  });

  it('pauses a projection run while the slider plays and runs again when it stops (§4.3)', async () => {
    const { l } = lenses(() => []);
    const posts: unknown[] = [];
    l.projectionHooks = {
      post: async (_path, body) => {
        posts.push(body);
        return {
          answer: {
            head: { vars: ['node', 'score'] },
            results: { bindings: [{ node: iri(`${EX}a`), score: lit('3') }] },
            summary: null,
            run: { commit: 1, cached: true, rows: 1 },
          },
        };
      },
    };
    l.setInput(input({ nodes: people, playing: true }));
    l.set('size', { lens: 'pageRank', scope: 'projection', projection: 'name:people' });
    await settle();
    expect(l.status.size.note).toBe('paused while playing');
    expect(posts).toHaveLength(0);
    l.setInput(input({ nodes: people, playing: false }));
    await settle();
    expect(posts).toHaveLength(1);
    expect(l.results.size?.values.get(`<${EX}a>`)).toBe(3);
    expect(l.results.size?.summary).toContain("server's kept run");
    l.destroy();
  });

  it('shows the change since the left handle in compare mode (§4.3)', async () => {
    const { l } = lenses((s, o) =>
      s.includes('COUNT(*)') ? [{ n: iri(`${EX}a`), c: lit(o.at === 'commit:1' ? '2' : '5') }] : [],
    );
    l.setInput(input({ at: 'commit:3', compareAt: 'commit:1' }));
    l.set('size', { lens: 'degree-dataset' });
    await settle();
    expect(l.results.size?.values.get(`<${EX}a>`)).toBe(5);
    expect(l.results.size?.text.get(`<${EX}a>`)).toBe('5, +3 since the left handle');
    l.destroy();
  });

  it('above 20,000 drawn nodes runs only a numeric projection lens, for the top 1,000 (§5)', async () => {
    const many = Array.from({ length: 20_001 }, (_, i) => node(`n${i}`));
    const { l } = lenses(() => []);
    const tops: unknown[] = [];
    l.projectionHooks = {
      post: async (_p, body) => {
        tops.push((body as { top?: number }).top);
        return {
          answer: {
            head: { vars: ['node', 'score'] },
            results: { bindings: [{ node: iri(`${EX}n7`), score: lit('9') }] },
            summary: null,
            run: { commit: 1, cached: false, rows: 1 },
          },
        };
      },
    };
    l.setInput(input({ nodes: many }));
    l.set('colour', { lens: 'class' });
    l.set('size', { lens: 'pageRank', scope: 'projection', projection: 'name:p' });
    await settleLong();
    expect(l.status.colour.note).toMatch(/at most 20,000/);
    // two requests a run (the background start and the read), each for the top 1,000
    expect(tops.length).toBeGreaterThanOrEqual(2);
    expect(tops.every((t) => t === 1000)).toBe(true);
    expect(l.results.size?.summary).toMatch(/^the top 1 of 20,001 drawn nodes/);
    l.destroy();
  });

  afterEach(() => vi.unstubAllGlobals());

  it('runs the GraphRAG lens again with fresh options after a past state (merge review)', async () => {
    let statusReads = 0;
    let projReads = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) => {
        const u = String(url);
        const json = (b: unknown) =>
          new Response(JSON.stringify(b), { headers: { 'Content-Type': 'application/json' } });
        if (u.includes('/memberships'))
          return json({
            available: true,
            memberships: [{ node: `${EX}a`, community: `${EX}c1` }],
            communities: [{ iri: `${EX}c1`, memberCount: 3, title: 'Team' }],
          });
        if (u.includes('/projections')) {
          projReads++;
          return json({ projections: [] });
        }
        if (/\/\$\/graphrag\/ds$/.test(new URL(u, 'http://x').pathname)) {
          statusReads++;
          return json({ enabled: true, available: true, levels: [{ level: 0 }] });
        }
        return new Response('{}', { status: 404 });
      }),
    );
    const { l } = lenses(() => []);
    l.setInput(input({ nodes: people }));
    await l.loadAlgoOptions();
    l.set('colour', { lens: 'graphrag' });
    await settleLong();
    expect(l.results.colour?.legend[0].text).toMatch(/^Team/);
    l.setInput(input({ nodes: people, at: 'commit:2', past: true }));
    await settleLong();
    expect(l.status.colour.error).toMatch(/past state/);
    // back at the head, the lens waits for the options read again and shows the communities
    l.setInput(input({ nodes: people }));
    await settleLong();
    expect(l.status.colour.error).toBeUndefined();
    expect(l.results.colour?.legend[0].text).toMatch(/^Team/);
    // a redraw of the dataset (a new epoch) reads the options again
    const before = statusReads + projReads;
    l.setInput(input({ nodes: people, epoch: 7 }));
    await settleLong();
    expect(statusReads + projReads).toBeGreaterThan(before);
    l.destroy();
  });

  it('refuses the GraphRAG lens at a past state although its options were read at the head (§6)', async () => {
    const fetches = vi.fn(async () => new Response('{}', { status: 500 }));
    vi.stubGlobal('fetch', fetches);
    const { l } = lenses(() => []);
    l.setInput(input({ nodes: people, at: 'commit:2', past: true }));
    // the options as read at the head, before the slider moved
    l.algo = { projections: [], projectionOff: null, graphrag: { levels: [0] }, graphragOff: null };
    l.set('colour', { lens: 'graphrag' });
    await settleLong();
    expect(l.status.colour.error).toMatch(/past state/);
    expect(fetches).not.toHaveBeenCalled();
    l.destroy();
  });

  it('keeps the status of a newer run when a cancel answers late (§2.3)', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(
        () =>
          new Promise<Response>((r) =>
            setTimeout(() => r(new Response('{}', { status: 202 })), 60),
          ),
      ),
    );
    const { l } = lenses(() => []);
    let n = 0;
    l.projectionHooks = {
      poll: 5,
      post: async (_p, body) => ({
        task: {
          id: `${(body as { projection: string }).projection}-${++n}`,
          state: 'running',
        } as unknown as Task,
      }),
      getTask: async (id) => ({ id, state: 'running', progress: 0.5 }) as unknown as Task,
    };
    l.setInput(input({ nodes: people }));
    l.set('size', { lens: 'pageRank', scope: 'projection', projection: 'name:a' });
    await settle();
    expect(l.status.size.task).toMatch(/^a-/);
    const cancelled = l.cancel('size');
    l.set('size', { lens: 'pageRank', scope: 'projection', projection: 'name:b' });
    await settle();
    expect(l.status.size.task).toMatch(/^b-/);
    await cancelled;
    expect(l.status.size.task).toMatch(/^b-/);
    expect(l.status.size.busy).toBe(true);
    expect(l.status.size.note).not.toBe('Cancelled.');
    l.destroy();
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
