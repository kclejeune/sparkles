import { afterEach, describe, expect, it, vi } from 'vitest';
import * as api from './api';

type Call = { url: string; init: RequestInit };

/** Replace fetch with a stub answering `respond(url, init)`; records the calls. */
function stubFetch(respond: (url: string, init: RequestInit) => Response | Promise<Response>) {
  const calls: Call[] = [];
  vi.stubGlobal('fetch', async (url: string, init: RequestInit = {}) => {
    calls.push({ url, init });
    return respond(url, init);
  });
  return calls;
}

const jsonResponse = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

afterEach(() => vi.unstubAllGlobals());

describe('errors', () => {
  it('parses the JSON error body with position', async () => {
    stubFetch(() =>
      jsonResponse({ error: 'Parse error', detail: 'unexpected }', line: 3, column: 7 }, 400),
    );
    const e = await api.query('ds', 'SELECT').catch((x) => x);
    expect(e).toBeInstanceOf(api.ApiError);
    expect(e.status).toBe(400);
    expect(e.message).toBe('Parse error');
    expect(e.line).toBe(3);
    expect(e.column).toBe(7);
    expect(api.errorMessage(e)).toBe('Parse error: unexpected }');
  });

  it('falls back to the status and body text', async () => {
    stubFetch(() => new Response('Service Unavailable', { status: 503 }));
    const e = await api.serverInfo().catch((x) => x);
    expect(e.status).toBe(503);
    expect(e.message).toBe('503 Service Unavailable');
  });

  it('reports network failures as status 0 but passes aborts through', async () => {
    stubFetch(() => Promise.reject(new TypeError('fetch failed')));
    const e = await api.listDatasets().catch((x) => x);
    expect(e.status).toBe(0);
    expect(api.errorMessage(e)).toContain('Cannot reach the Sparkles server');

    stubFetch(() => Promise.reject(new DOMException('aborted', 'AbortError')));
    const a = await api.listDatasets().catch((x) => x);
    expect(a).toBeInstanceOf(DOMException);
  });

  it('errorMessage does not repeat an identical detail', () => {
    expect(api.errorMessage(new api.ApiError(500, 'boom', { detail: 'boom' }))).toBe('boom');
    expect(api.errorMessage(new Error('plain'))).toBe('plain');
    expect(api.errorMessage('text')).toBe('text');
  });
});

describe('format', () => {
  it('posts the text, language and cursor and returns the result', async () => {
    const result = {
      text: 'ASK {}',
      changed: false,
      language: 'sparql',
      cursorOffset: 3,
      warnings: [],
    };
    const calls = stubFetch(() => jsonResponse(result));
    const r = await api.format({ text: 'ASK {}', language: 'sparql', cursorOffset: 3 });
    expect(r).toEqual(result);
    expect(calls[0].url).toBe('/$/format');
    expect(calls[0].init.method).toBe('POST');
    expect(JSON.parse(String(calls[0].init.body))).toEqual({
      text: 'ASK {}',
      language: 'sparql',
      cursorOffset: 3,
    });
  });

  it('throws syntax errors with their code and position', async () => {
    stubFetch(() =>
      jsonResponse(
        {
          error: 'SPARQL syntax error at line 1, column 11: expected …',
          code: 'syntax',
          language: 'sparql',
          line: 1,
          column: 11,
          requestId: 'r1',
        },
        400,
      ),
    );
    const e = await api.format({ text: 'select * {' }).catch((x) => x);
    expect(e).toBeInstanceOf(api.ApiError);
    expect(e.code).toBe('syntax');
    expect(e.line).toBe(1);
    expect(e.column).toBe(11);
  });
});

describe('query', () => {
  it('encodes the dataset and options and strips ?-prefixed variable names', async () => {
    const calls = stubFetch(() =>
      jsonResponse({ queryType: 'SELECT', vars: ['?s', '$o'], rows: [] }),
    );
    const r = await api.query('my ds', 'SELECT ?s ?o {}', {
      send: 10,
      reasoning: false,
      nocache: true,
    });
    expect(calls[0].url).toBe('/my%20ds/sparql?nocache=true&send=10&reasoning=false');
    expect((calls[0].init.headers as Record<string, string>).Accept).toBe(api.SPARKLES_JSON);
    expect(r.vars).toEqual(['s', 'o']);
    expect(r.meta.totalRows).toBe(0);
  });

  it('select maps rows to objects with undefined for unbound', async () => {
    stubFetch(() =>
      jsonResponse({
        queryType: 'SELECT',
        vars: ['s', 'o'],
        rows: [[{ type: 'uri', value: 'http://a' }, null]],
      }),
    );
    expect(await api.select('ds', 'SELECT * {}')).toEqual([
      { s: { type: 'uri', value: 'http://a' }, o: undefined },
    ]);
  });

  it('update returns counts, or null for a non-JSON body', async () => {
    stubFetch(() => jsonResponse({ inserted: 2, deleted: 1, operations: 1 }));
    expect(await api.update('ds', 'INSERT DATA {}')).toMatchObject({ inserted: 2, deleted: 1 });
    stubFetch(() => new Response('<html>ok</html>'));
    expect(await api.update('ds', 'INSERT DATA {}')).toBeNull();
  });

  it('update asks for a receipt and returns it', async () => {
    const commit = {
      seq: 42,
      parent: 41,
      ref: 'commit:42',
      timestamp: '2026-09-30T16:10:15.821Z',
      kind: 'update',
      inserted: 2,
      deleted: 0,
      quads: 182,
      generation: 'gen-0002',
      bulk: false,
      exact: true,
    };
    const calls = stubFetch(() =>
      jsonResponse({
        inserted: 2,
        deleted: 0,
        operations: 1,
        dataset: 'ds',
        datasetId: 'f436',
        committed: true,
        commit,
      }),
    );
    const r = await api.update('ds', 'INSERT DATA {}');
    expect(calls[0].url).toBe('/ds/update?receipt=true');
    expect(r?.receipt).toEqual({ dataset: 'ds', datasetId: 'f436', committed: true, commit });
    // no receipt members (older server): none reported
    stubFetch(() => jsonResponse({ inserted: 0, deleted: 0, operations: 1 }));
    expect((await api.update('ds', 'INSERT DATA {}'))?.receipt).toBeUndefined();
  });

  it('rejects invalid JSON from a JSON endpoint (schema)', async () => {
    stubFetch(() => new Response('{nope'));
    const e = await api.schemaSummary('ds').catch((x) => x);
    expect(e.message).toBe('Server returned invalid JSON');
  });

  it('rejects invalid JSON from a JSON endpoint', async () => {
    stubFetch(() => new Response('{nope'));
    const e = await api.serverInfo().catch((x) => x);
    expect(e.message).toBe('Server returned invalid JSON');
  });
});

describe('schema', () => {
  /** A fake schema server: `items` classes in pages of `limit`; `version` can change. */
  function fakeSchema(items: string[], limit: number) {
    const state = { version: 1, served: new Set<number>([]) };
    const page = (after: number, v: number) => {
      const slice = items.slice(after, after + limit);
      const end = after + slice.length;
      return {
        items: slice.map((iri) => ({ iri })),
        total: items.length,
        next: end < items.length ? `${v}:${end}` : null,
      };
    };
    const calls = stubFetch((url) => {
      const u = new URL(url, 'http://x');
      const cursor = u.searchParams.get('cursor');
      if (!cursor) {
        state.served.add(state.version);
        return jsonResponse({
          schemaFormat: 1,
          snapshot: { version: state.version },
          classes: page(0, state.version),
          predicates: { items: [{ iri: 'p' }], total: 1, next: null },
        });
      }
      const [v, after] = cursor.split(':').map(Number);
      if (v !== state.version) return jsonResponse({ error: 'snapshot changed' }, 409);
      return jsonResponse(page(after, v));
    });
    return { state, calls };
  }

  it('follows next until the lists are complete', async () => {
    const items = Array.from({ length: 7 }, (_, i) => `c${i}`);
    const { calls } = fakeSchema(items, 3);
    const s = await api.schema('my ds', { graph: 'union', reasoning: false, limit: 3 });
    expect(s.classes.items.map((c) => c.iri)).toEqual(items);
    expect(s.classes.next).toBeNull();
    expect(s.predicates.items).toHaveLength(1);
    expect(calls.map((c) => c.url)).toEqual([
      '/$/schema/my%20ds?graph=union&reasoning=false&limit=3',
      '/$/schema/my%20ds/classes?graph=union&reasoning=false&limit=3&cursor=1%3A3',
      '/$/schema/my%20ds/classes?graph=union&reasoning=false&limit=3&cursor=1%3A6',
    ]);
  });

  // The snapshot changes between pages and the server no longer holds the old report:
  // start over rather than mixing pages of two snapshots.
  it('restarts from the first page on 409', async () => {
    const { state } = fakeSchema(['a', 'b', 'c'], 2);
    const fake = globalThis.fetch;
    let n = 0;
    vi.stubGlobal('fetch', async (url: string, init?: RequestInit) => {
      const r = await fake(url, init);
      if (n++ === 0) state.version = 2; // a write lands after the first page
      return r;
    });
    const s = await api.schema('ds');
    expect(s.snapshot.version).toBe(2);
    expect(s.classes.items.map((c) => c.iri)).toEqual(['a', 'b', 'c']);
    expect([...state.served]).toEqual([1, 2]);
  });

  it('gives up after the allowed attempts', async () => {
    stubFetch((url) =>
      url.includes('cursor')
        ? jsonResponse({ error: 'snapshot changed' }, 409)
        : jsonResponse({
            snapshot: { version: 1 },
            classes: { items: [], total: 1, next: 'x' },
            predicates: { items: [], total: 0, next: null },
          }),
    );
    const e = await api.schema('ds', {}, 2).catch((x) => x);
    expect(e).toBeInstanceOf(api.ApiError);
    expect(e.status).toBe(409);
  });

  it('does not retry other errors', async () => {
    const calls = stubFetch(() => jsonResponse({ error: 'no such graph' }, 404));
    const e = await api.schema('ds', { graph: 'http://ex.org/g' }).catch((x) => x);
    expect(e.status).toBe(404);
    expect(calls).toHaveLength(1);
  });
});

describe('commits and full-text admin', () => {
  it('pages the commit catalog with before= and limit=', async () => {
    const calls = stubFetch(() =>
      jsonResponse({ head: 3, firstRetained: 0, complete: true, commits: [], next: null }),
    );
    await api.commits('my ds', { before: 20, limit: 25 });
    expect(calls[0].url).toBe('/$/commits/my%20ds?limit=25&before=20');
    await api.commits('ds');
    expect(calls[1].url).toBe('/$/commits/ds');
  });

  it('reads the text status, null when disabled', async () => {
    stubFetch(() => jsonResponse({ enabled: false }));
    expect(await api.textStatus('ds')).toBeNull();
    stubFetch(() => jsonResponse({ enabled: true, state: 'ready', docs: 3 }));
    expect(await api.textStatus('ds')).toMatchObject({ state: 'ready', docs: 3 });
    for (const state of ['rebuilding', 'failed'] as const) {
      stubFetch(() => jsonResponse({ enabled: true, state, docs: 0, message: state }));
      expect(await api.textStatus('ds')).toMatchObject({ state, message: state });
    }
    stubFetch(() => jsonResponse({ error: 'built without full-text search' }, 501));
    const e = await api.textStatus('ds').catch((x) => x);
    expect(e.status).toBe(501);
  });

  it('enables with PUT and a JSON config, rebuilds with POST', async () => {
    const calls = stubFetch(() =>
      jsonResponse({ id: '1', kind: 'text-rebuild', state: 'running' }, 202),
    );
    const t = await api.enableText('ds', { predicates: ['http://ex/label'] });
    expect(t.kind).toBe('text-rebuild');
    expect(calls[0].init.method).toBe('PUT');
    expect(JSON.parse(String(calls[0].init.body))).toEqual({ predicates: ['http://ex/label'] });
    await api.rebuildText('ds');
    expect(calls[1].url).toBe('/$/text/ds/rebuild');
    expect(calls[1].init.method).toBe('POST');
  });

  it('extracts receipts only from bodies that have them', () => {
    expect(api.receiptOf({ count: 3 })).toBeUndefined();
    expect(api.receiptOf({ committed: true, commit: 'x' })).toBeUndefined();
    expect(
      api.receiptOf({ committed: false, dataset: 'd', datasetId: 'i', commit: { seq: 4 } })?.commit
        .seq,
    ).toBe(4);
  });
});

describe('CSRF', () => {
  afterEach(() =>
    api.setAuthHooks({ csrf: () => undefined, unauthorized: () => {}, ready: async () => {} }),
  );

  it('an unsafe request made before the caller is known waits for its CSRF token', async () => {
    let token: string | undefined;
    let loaded!: () => void;
    const whoami = new Promise<void>((r) => (loaded = r));
    api.setAuthHooks({ csrf: () => token, ready: () => whoami });
    const calls = stubFetch(() => jsonResponse({ head: { vars: [] }, results: { bindings: [] } }));

    const q = api.query('ds', 'SELECT * WHERE { ?s ?p ?o }');
    await new Promise((r) => setTimeout(r, 0));
    expect(calls).toHaveLength(0);
    token = 'csrf-1';
    loaded();
    await q;
    expect(new Headers(calls[0].init.headers).get('X-Sparkles-CSRF')).toBe('csrf-1');
  });

  it('safe requests do not wait', async () => {
    api.setAuthHooks({ ready: () => new Promise(() => {}) });
    const calls = stubFetch(() => new Response('2026-01-01T00:00:00Z'));
    await api.ping();
    expect(calls).toHaveLength(1);
  });
});

describe('shex', () => {
  it('posts the envelope with the options as parameters', async () => {
    const report: api.ShexReport = {
      conforms: true,
      counts: { conformant: 1, nonconformant: 0 },
      results: [],
      warnings: [],
      millis: 1,
    };
    const calls = stubFetch(() => jsonResponse(report));
    const r = await api.shex('my ds', 'ex:S {}', '{FOCUS a ex:C}@ex:S', {
      graph: 'union',
      reasoning: false,
      onlyNonconformant: true,
    });
    expect(r.counts.conformant).toBe(1);
    const u = new URL(calls[0].url, 'http://x');
    expect(u.pathname).toBe('/my%20ds/shex');
    expect(u.searchParams.get('graph')).toBe('union');
    expect(u.searchParams.get('reasoning')).toBe('false');
    expect(u.searchParams.get('results')).toBe('nonconformant');
    expect(u.searchParams.get('format')).toBeNull();
    expect(JSON.parse(String(calls[0].init.body))).toEqual({
      schema: 'ex:S {}',
      map: '{FOCUS a ex:C}@ex:S',
    });
  });

  it('asks for other formats by name', async () => {
    const calls = stubFetch(() => jsonResponse([]));
    await api.shexRaw('ds', 's', 'm', 'shapemap');
    expect(new URL(calls[0].url, 'http://x').searchParams.get('format')).toBe('shapemap');
  });
});
