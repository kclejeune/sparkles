import test from 'node:test';
import assert from 'node:assert/strict';
import { SparklesClient, NotFoundError, factory } from '../dist/index.js';

const body = {
  head: { vars: ['s', 'x'] },
  results: {
    bindings: [
      {
        s: { type: 'uri', value: 'urn:s' },
        x: { type: 'literal', value: 'héllo', 'xml:lang': 'fr' },
      },
      { s: { type: 'bnode', value: 'b1f' } },
    ],
  },
};
test('SELECT streams UTF-8 JSON split at every byte and releases on early break', async () => {
  let cancelled = false;
  let request;
  const client = new SparklesClient('http://example.test', {
    fetch: async (input, init) => {
      request = new Request(input, init);
      const bytes = new TextEncoder().encode(JSON.stringify(body));
      let at = 0;
      return new Response(
        new ReadableStream({
          pull(c) {
            if (at < bytes.length) c.enqueue(bytes.slice(at, ++at));
            else c.close();
          },
          cancel() {
            cancelled = true;
          },
        }),
        { headers: { 'content-type': 'application/sparql-results+json' } },
      );
    },
  });
  const result = await client.dataset('wiki').select('SELECT ?s ?x WHERE {?s ?p ?x}');
  assert.deepEqual(
    result.variables.map((v) => v.value),
    ['s', 'x'],
  );
  for await (const row of result) {
    assert(row.get('s').equals(factory.namedNode('urn:s')));
    assert.equal(row.get('x').value, 'héllo');
    break;
  }
  assert(cancelled);
  assert.equal(request.method, 'POST');
  assert.equal(request.url, 'http://example.test/wiki/sparql');
});
test('ASK, write receipts and typed admin fetch share authentication', async () => {
  const requests = [];
  const client = new SparklesClient('http://example.test', {
    token: 'secret',
    fetch: async (input, init) => {
      const request = new Request(input, init);
      requests.push(request);
      if (request.url.includes('update'))
        return Response.json({
          inserted: 1,
          deleted: 0,
          receipt: {
            datasetId: 'test',
            committed: true,
            commit: { seq: 1, kind: 'update', inserted: 1, deleted: 0 },
          },
        });
      if (request.url.includes('sparql')) return Response.json({ boolean: true });
      return Response.json({ name: 'wiki', type: 'mem', endpoints: {}, quads: 1 });
    },
  });
  assert(await client.dataset('wiki').ask('ASK {}'));
  const r = await client.dataset('wiki').update('INSERT DATA {}');
  assert.equal(r.receipt.commit.seq, 1n);
  await client.api.GET('/$/datasets/{name}', { params: { path: { name: 'wiki' } } });
  assert(requests.every((r) => r.headers.get('authorization') === 'Bearer secret'));
});
test('404 means NotFound and unsafe updates are never retried', async () => {
  let calls = 0;
  const client = new SparklesClient('http://example.test', {
    fetch: async () => {
      calls++;
      return Response.json({ error: 'missing dataset' }, { status: 404 });
    },
  });
  await assert.rejects(client.dataset('wiki').update('INSERT DATA {}'), NotFoundError);
  assert.equal(calls, 1);
});
test('CONSTRUCT reads N-Quads literals and named graphs', async () => {
  const client = new SparklesClient('http://example.test', {
    fetch: async () =>
      new Response('<urn:s> <urn:p> "x"@en <urn:g> .\n', {
        headers: { 'content-type': 'application/n-quads' },
      }),
  });
  const q = (
    await (
      await client.dataset('wiki').construct('CONSTRUCT {?s ?p ?o} WHERE {?s ?p ?o}')
    ).toArray()
  )[0];
  assert.equal(q.object.language, 'en');
  assert.equal(q.graph.value, 'urn:g');
});
