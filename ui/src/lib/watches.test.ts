import { afterEach, describe, expect, it, vi } from 'vitest';
import * as w from './watches';

const watch = (o: Partial<w.Watch> = {}): w.Watch => ({
  id: 'w1',
  dataset: 'uni',
  branch: 'main',
  entity: 'http://example.org/uni#Professor',
  scope: 'entity',
  created: '2026-11-01T00:00:00Z',
  since: 3,
  seen: 3,
  unread: 0,
  overflow: false,
  truncated: false,
  ...o,
});

describe('watch helpers', () => {
  it('labels a scope with its depth', () => {
    expect(w.scopeLabel({ scope: 'entity' })).toBe('Entity');
    expect(w.scopeLabel({ scope: 'subtree' })).toBe('Subtree');
    expect(w.scopeLabel({ scope: 'subtree', depth: 1 })).toBe('Subtree, 1 level');
    expect(w.scopeLabel({ scope: 'subtree', depth: 3 })).toBe('Subtree, 3 levels');
  });

  it('parses a depth from 1 to 1000 or none', () => {
    expect(w.parseDepth('')).toBeUndefined();
    expect(w.parseDepth(' 2 ')).toBe(2);
    expect(w.parseDepth('1000')).toBe(1000);
    for (const bad of ['0', '1001', '-1', '1.5', 'two']) expect(w.parseDepth(bad)).toBe('invalid');
  });

  it('finds the watches on one entity, branch and graph', () => {
    const ws = [
      watch(),
      watch({ id: 'w2', scope: 'subtree' }),
      watch({ id: 'w3', branch: 'dev' }),
      watch({ id: 'w4', graph: 'urn:g' }),
    ];
    const t = { dataset: 'uni', branch: 'main', graph: null, entity: watch().entity };
    expect(w.findWatch(ws, t).map((x) => x.id)).toEqual(['w1', 'w2']);
    expect(w.findWatch(ws, { ...t, graph: 'urn:g' }).map((x) => x.id)).toEqual(['w4']);
  });

  it('says what a match lists', () => {
    const m = { commit: 4, time: '', count: 1, exact: true, unread: true };
    expect(w.matchText({ ...m, members: ['urn:a'] })).toBe('');
    expect(w.matchText({ ...m, members: ['urn:a'], count: 30 })).toBe('1 shown of 30');
    expect(w.matchText({ ...m, summary: true, count: 1500 })).toBe(
      '1500 changes under the watched entity',
    );
    expect(w.matchText({ ...m, exact: false })).toContain('not listed');
  });
});

describe('watch requests', () => {
  afterEach(() => vi.unstubAllGlobals());

  function capture(body: unknown = {}) {
    const calls: { url: string; init: RequestInit }[] = [];
    vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
      calls.push({ url, init });
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'Content-Type': 'application/json' },
      });
    });
    return calls;
  }

  it('lists by dataset, entity and graph, with default for the default graph', async () => {
    const calls = capture({ watches: [], unread: 0 });
    await w.listWatches({ dataset: 'uni', entity: 'urn:a#b', graph: null });
    expect(calls[0].url).toBe('/$/watches?dataset=uni&entity=urn%3Aa%23b&graph=default');
    await w.listWatches();
    expect(calls[1].url).toBe('/$/watches');
  });

  it('creates, marks read and deletes', async () => {
    const calls = capture(watch());
    await w.createWatch({
      dataset: 'uni',
      entity: 'urn:a',
      graph: null,
      scope: 'subtree',
      depth: 2,
    });
    expect(calls[0].init.method).toBe('POST');
    expect(JSON.parse(String(calls[0].init.body))).toEqual({
      dataset: 'uni',
      entity: 'urn:a',
      scope: 'subtree',
      depth: 2,
    });
    await w.markSeen('a/b', 9);
    expect(calls[1].url).toBe('/$/watches/a%2Fb/seen');
    expect(JSON.parse(String(calls[1].init.body))).toEqual({ commit: 9 });
    await w.watchMatches('w1', { before: 7, limit: 50 });
    expect(calls[2].url).toBe('/$/watches/w1/matches?before=7&limit=50');
    await w.deleteWatch('w1');
    expect(calls[3].init.method).toBe('DELETE');
  });
});
