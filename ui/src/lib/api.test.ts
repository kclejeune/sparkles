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

  it('rejects invalid JSON from a JSON endpoint', async () => {
    stubFetch(() => new Response('{nope'));
    const e = await api.serverInfo().catch((x) => x);
    expect(e.message).toBe('Server returned invalid JSON');
  });
});
