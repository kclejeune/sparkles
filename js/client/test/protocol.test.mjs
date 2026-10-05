import test from 'node:test';
import assert from 'node:assert/strict';
import {
  SparklesClient,
  factory,
  CancelledError,
  QueryTimeoutError,
  RdfSyntaxError,
  UnsupportedError,
  InvalidInputError,
  WriteRejectedError,
  ConflictError,
} from '../dist/index.js';

const jsonHeaders = { 'content-type': 'application/sparql-results+json' };
const ask = () => Response.json({ head: {}, boolean: true }, { headers: jsonHeaders });
const binding = { s: { type: 'uri', value: 'urn:s' } };
test('remote dry runs reject updates, batches and graph writes before dispatch', async () => {
  let calls = 0;
  const fetch = async () => {
    calls++;
    return new Response(null, { status: 204 });
  };
  const endpoints = [
    dataset(fetch),
    SparklesClient.endpoint('http://example.test/query', {
      fetch,
      updateUrl: 'http://example.test/update',
      graphStoreUrl: 'http://example.test/data',
    }),
  ];
  for (const ds of endpoints)
    for (const dryRun of [true, { changes: 10 }]) {
      const options = { dryRun };
      await assert.rejects(ds.update('INSERT DATA {<urn:s> <urn:p> 1}', options), UnsupportedError);
      await assert.rejects(
        ds.batch(() => assert.fail('Unsupported batch must not invoke its callback'), options),
        UnsupportedError,
      );
      await assert.rejects(
        ds.graphs.put(undefined, '<urn:s> <urn:p> 1 .', options),
        UnsupportedError,
      );
      await assert.rejects(
        ds.graphs.post(undefined, '<urn:s> <urn:p> 1 .', options),
        UnsupportedError,
      );
      await assert.rejects(ds.graphs.delete(undefined, options), UnsupportedError);
    }
  assert.equal(calls, 0);
  await endpoints[0].update('INSERT DATA {}', { dryRun: false });
  assert.equal(calls, 1);
});
function stream(text, { chunk = 1, hang = false } = {}) {
  const bytes = typeof text === 'string' ? new TextEncoder().encode(text) : text;
  let at = 0,
    cancels = 0;
  const body = new ReadableStream({
    pull(c) {
      if (at < bytes.length) {
        c.enqueue(bytes.slice(at, (at += chunk)));
      } else if (!hang) c.close();
    },
    cancel() {
      cancels++;
    },
  });
  return {
    body,
    get cancels() {
      return cancels;
    },
  };
}
function dataset(fetch, options = {}) {
  return new SparklesClient('http://example.test', { fetch, ...options }).dataset('wiki', 'work');
}
async function consumed(text) {
  const ds = dataset(async () => new Response(text, { headers: jsonHeaders }));
  const result = await ds.query('SELECT * WHERE {}');
  if (result.type === 'bindings') await result.toArray();
  return result;
}

test('plain endpoint preserves existing parameters and adds valid BASE/PREFIX declarations to reads and writes', async () => {
  const requests = [];
  const ds = SparklesClient.endpoint(
    'https://example.test/query?tenant=a&default-graph-uri=urn:old',
    {
      updateUrl: 'https://example.test/update?tenant=a',
      fetch: async (input) => {
        const request = new Request(input);
        requests.push({ url: new URL(request.url), body: await request.text() });
        return request.url.includes('/update') ? new Response(null, { status: 204 }) : ask();
      },
    },
  );
  await ds.ask('ASK {}', {
    baseIri: 'urn:base:',
    prefixes: { ex: 'urn:ex:' },
    defaultGraph: ['urn:new'],
    namedGraphs: ['urn:g'],
  });
  assert.equal(requests[0].url.searchParams.get('tenant'), 'a');
  assert.deepEqual(requests[0].url.searchParams.getAll('default-graph-uri'), ['urn:new']);
  assert.equal(requests[0].url.searchParams.get('named-graph-uri'), 'urn:g');
  assert.equal(requests[0].body, 'BASE <urn:base:>\nPREFIX ex: <urn:ex:>\nASK {}');
  await ds.update('INSERT DATA {}', { baseIri: 'urn:base:', prefixes: { ex: 'urn:ex:' } });
  assert.equal(requests[1].url.searchParams.get('tenant'), 'a');
  assert(requests[1].body.startsWith('BASE <urn:base:>\nPREFIX ex: <urn:ex:>'));
});
test('invalid prefix and IRI input is rejected before sending a request', async () => {
  let calls = 0;
  const ds = dataset(async () => {
    calls++;
    return ask();
  });
  for (const options of [
    { prefixes: { 'x:> INSERT DATA {': 'urn:x' } },
    { prefixes: { _: 'urn:x' } },
    { prefixes: { 'x.': 'urn:x' } },
    { prefixes: { x: 'urn:x>\nASK {}' } },
    { baseIri: 'relative' },
    { namedGraphs: ['urn:x%GG'] },
  ])
    await assert.rejects(ds.query('ASK {}', options), InvalidInputError);
  assert.equal(calls, 0);
});
test('Sparkles query parameters carry snapshot, inferred flag, budgets and DESCRIBE settings on a branch', async () => {
  let url;
  const ds = dataset(async (input) => {
    url = new URL(input.url);
    return ask();
  });
  await ds.ask('ASK {}', {
    at: 4n,
    timeout: 2000,
    includeInferred: false,
    maxRows: 2,
    maxRowsProduced: 3,
    maxMemoryBytes: 2 << 20,
    describe: { mode: 'scbd', labels: true, reifiers: false, maxTriples: 10, maxDepth: 2 },
  });
  assert.equal(url.pathname, '/wiki/sparql');
  assert.equal(url.searchParams.get('branch'), 'work');
  assert.equal(url.searchParams.get('at'), 'commit:4');
  assert.equal(url.searchParams.get('reasoning'), 'false');
  assert.equal(url.searchParams.get('timeout'), '2');
  for (const [key, value] of Object.entries({
    'max-rows': '2',
    'max-rows-produced': '3',
    'memory-mb': '2',
    describe: 'scbd',
    'describe-labels': 'true',
    'describe-reifiers': 'false',
    'describe-max-triples': '10',
    'describe-max-depth': '2',
  }))
    assert.equal(url.searchParams.get(key), value);
  await assert.rejects(ds.ask('ASK {}', { maxMemoryBytes: 4 }), UnsupportedError);
  await assert.rejects(ds.ask('ASK {}', { describe: { arbitrary: true } }), UnsupportedError);
  await assert.rejects(ds.ask('ASK {}', { maxRows: 0 }), InvalidInputError);
});
test('unsupported controls never silently disappear on plain endpoints or graph writes', async () => {
  let calls = 0;
  const fetch = async () => {
    calls++;
    return ask();
  };
  const plain = SparklesClient.endpoint('http://example.test/query', { fetch });
  for (const options of [
    { at: 1 },
    { includeInferred: false },
    { maxRows: 1 },
    { describe: 'cbd' },
    { batchBytes: 1024 },
    { bindings: {} },
  ])
    await assert.rejects(plain.query('ASK {}', options), UnsupportedError);
  const ds = dataset(fetch);
  for (const options of [{ noWait: true }, { ifHead: 1n }]) {
    await assert.rejects(ds.update('INSERT DATA {}', options), UnsupportedError);
    await assert.rejects(ds.graphs.delete(factory.defaultGraph(), options), UnsupportedError);
  }
  await assert.rejects(
    ds.graphs.put(factory.blankNode('x'), '', { contentType: 'text/turtle' }),
    InvalidInputError,
  );
  await assert.rejects(ds.graphs.get(undefined, { includeInferred: false }), UnsupportedError);
  assert.equal(calls, 0);
});
test('graph writes forward message, media type, ETag and receipt while branch commits page correctly', async () => {
  const requests = [];
  const client = new SparklesClient('http://example.test', {
    fetch: async (input) => {
      const r = new Request(input);
      requests.push(r);
      return r.url.includes('/$/commits')
        ? Response.json({ commits: [], next: null })
        : Response.json({
            datasetId: 'id',
            committed: true,
            commit: { seq: 7, kind: 'load', inserted: 2, deleted: 0 },
          });
    },
  });
  const ds = client.dataset('wiki', 'dev /+');
  const receipt = await ds.graphs.put(factory.namedNode('urn:g'), '<urn:s> <urn:p> 1 .', {
    contentType: 'text/turtle',
    ifMatch: 'W/"id-6-ttl"',
    message: 'hello é',
  });
  assert.equal(receipt.commit.seq, 7n);
  assert.equal(requests[0].headers.get('if-match'), 'W/"id-6-ttl"');
  assert.equal(requests[0].headers.get('content-type'), 'text/turtle');
  assert.equal(requests[0].headers.get('sparkles-commit-message'), "UTF-8''hello%20%C3%A9");
  const graphUrl = new URL(requests[0].url);
  assert.equal(graphUrl.pathname, '/wiki/data');
  assert.equal(graphUrl.searchParams.get('branch'), 'dev /+');
  assert.equal(graphUrl.searchParams.get('graph'), 'urn:g');
  assert.equal(graphUrl.searchParams.get('receipt'), 'true');
  await ds.graphs.delete(undefined, { message: 'delete' });
  assert.equal(requests[1].headers.get('sparkles-commit-message'), "UTF-8''delete");
  await ds.commits({ limit: 2, before: 7n });
  const history = new URL(requests[2].url);
  assert.equal(history.pathname, '/$/commits/wiki');
  assert.equal(history.searchParams.get('branch'), 'dev /+');
  assert.equal(history.searchParams.get('before'), '7');
  assert.equal(history.searchParams.get('limit'), '2');
  await assert.rejects(ds.commits({ before: 1, after: 2 }), InvalidInputError);
});
test('remote unsafe counts and receipt sequence numbers are rejected instead of silently rounded', async () => {
  const ds = dataset(async () => Response.json({ inserted: 9007199254740992, deleted: 0 }));
  await assert.rejects(ds.update('INSERT DATA {}'), InvalidInputError);
  const unsafeReceipt = dataset(async () =>
    Response.json({
      datasetId: 'id',
      committed: true,
      commit: { seq: 9007199254740992, kind: 'update' },
    }),
  );
  await assert.rejects(unsafeReceipt.update('INSERT DATA {}'), InvalidInputError);
});
test('synchronous batches send one update request and refuse asynchronous callbacks', async () => {
  let body,
    calls = 0;
  const ds = dataset(async (input) => {
    calls++;
    body = await input.text();
    return Response.json({ inserted: 2, deleted: 0 });
  });
  await ds.batch((batch) => {
    batch.update('INSERT DATA {}');
    batch.update('DELETE DATA {}');
  });
  assert.equal(body, 'INSERT DATA {};\nDELETE DATA {}');
  assert.equal(calls, 1);
  await assert.rejects(
    ds.batch(async (batch) => {
      batch.update('INSERT DATA {}');
    }),
    UnsupportedError,
  );
  assert.equal(calls, 1);
});
test('safe requests retry bounded status errors and cancel failed response bodies; unsafe requests do not retry', async () => {
  let calls = 0,
    cancels = 0;
  const ds = dataset(
    async () => {
      calls++;
      if (calls < 3)
        return new Response(
          new ReadableStream({
            cancel() {
              cancels++;
            },
          }),
          { status: 503, headers: { 'retry-after': '0' } },
        );
      return ask();
    },
    { maxRetries: 2 },
  );
  assert(await ds.ask('ASK {}'));
  assert.equal(calls, 3);
  assert.equal(cancels, 2);
  let writes = 0;
  const unsafe = dataset(async () => {
    writes++;
    return Response.json({ error: 'busy' }, { status: 503 });
  });
  await assert.rejects(unsafe.update('INSERT DATA {}'));
  assert.equal(writes, 1);
});
test('abort during retry wait stops another request', { timeout: 1500 }, async () => {
  const controller = new AbortController();
  let calls = 0;
  const ds = dataset(async () => {
    calls++;
    setTimeout(() => controller.abort(), 10);
    return new Response(null, { status: 503, headers: { 'retry-after': '1' } });
  });
  await assert.rejects(ds.ask('ASK {}', { signal: controller.signal }), CancelledError);
  assert.equal(calls, 1);
});
test(
  'request abort and local timeout work when an injected fetch never settles',
  { timeout: 1500 },
  async () => {
    const controller = new AbortController();
    const ds = dataset(() => new Promise(() => {}));
    const waiting = ds.ask('ASK {}', { signal: controller.signal });
    controller.abort();
    await assert.rejects(waiting, CancelledError);
    const hold = setTimeout(() => {}, 1000);
    try {
      await assert.rejects(ds.ask('ASK {}', { timeout: 10 }), QueryTimeoutError);
    } finally {
      clearTimeout(hold);
    }
  },
);
test(
  'abort after response headers cancels a blocked query reader and preserves cancellation type',
  { timeout: 1500 },
  async () => {
    const input = stream('{"head":{"vars":["s"]},"results":{"bindings":[', {
      hang: true,
      chunk: 99,
    });
    const controller = new AbortController();
    const ds = dataset(async () => new Response(input.body, { headers: jsonHeaders }));
    const waiting = ds.select('SELECT * WHERE {}', { signal: controller.signal });
    setTimeout(() => controller.abort(), 10);
    await assert.rejects(waiting, CancelledError);
    assert.equal(input.cancels, 1);
    assert.equal(input.body.locked, false);
  },
);
test(
  'close races with pending next and queued next, cancels once and returns no late rows',
  { timeout: 1500 },
  async () => {
    const input = stream(
      '{"head":{"vars":["s"]},"results":{"bindings":[' + JSON.stringify(binding) + ',',
      { hang: true, chunk: 999 },
    );
    const result = await dataset(
      async () => new Response(input.body, { headers: jsonHeaders }),
    ).select('SELECT * WHERE {}');
    assert.equal((await result.next()).value.get('s').value, 'urn:s');
    const pending = result.next(),
      queued = result.next();
    await Promise.all([result.close(), result.close()]);
    assert.equal((await pending).done, true);
    assert.equal((await queued).done, true);
    assert.equal(input.cancels, 1);
    assert.equal(input.body.locked, false);
  },
);
test('priming malformed JSON cancels and releases reader ownership', async () => {
  const input = stream('{"head":{},"results":{"bindings":[null]}}', { hang: true, chunk: 99 });
  await assert.rejects(
    dataset(async () => new Response(input.body, { headers: jsonHeaders })).select(
      'SELECT * WHERE {}',
    ),
    RdfSyntaxError,
  );
  assert.equal(input.cancels, 1);
  assert.equal(input.body.locked, false);
});
test('malformed and mixed SPARQL JSON are rejected including errors after streamed rows', async () => {
  const invalid = [
    '{"boolean":true',
    '{"boolean":true}garbage',
    '{"boolean":1}',
    '{"boolean":true,"results":{"bindings":[]}}',
    '{"results":{"bindings":[]},"boolean":false}',
    '{"results":{"bindings":[],}}',
    '{"results":{"bindings":[{},]}}',
    '{"results":{"bindings":[]},}',
    '{"head":{"vars":["s","s"]},"results":{"bindings":[]}}',
    '{"head":{"vars":["s"]},"results":{"bindings":[{"x":{"type":"uri","value":"urn:x"}}]}}',
    '{"results":{"bindings":[{"s":{"type":"uri","value":"urn:s"} "x":null}]}}',
    '{"boolean":true,"boolean":false}',
    '{"results":{"bindings":[{"s":{"type":"uri","value":"relative"}}]}}',
    '{"head":{},"other":{"a":1,"a":2},"boolean":true}',
  ];
  for (const document of invalid)
    await assert.rejects(consumed(document), RdfSyntaxError, document);
});
test('empty SELECT, late head, extra metadata, and RDF1.2 JSON terms use supplied factories', async () => {
  const marker = new WeakSet();
  const custom = {
    ...factory,
    namedNode(value) {
      const term = factory.namedNode(value);
      marker.add(term);
      return term;
    },
    variable(value) {
      const term = factory.variable(value);
      marker.add(term);
      return term;
    },
  };
  const text = JSON.stringify({
    results: {
      bindings: [
        {
          s: {
            type: 'triple',
            value: {
              subject: { type: 'uri', value: 'urn:s' },
              predicate: { type: 'uri', value: 'urn:p' },
              object: { type: 'literal', value: 'é', 'xml:lang': 'fr', 'its:dir': 'rtl' },
            },
          },
        },
      ],
    },
    head: { vars: ['s'] },
    extension: { nested: [1, null] },
  });
  const result = await dataset(async () => new Response(text, { headers: jsonHeaders })).select(
    'SELECT * WHERE {}',
    { factory: custom },
  );
  const rows = await result.toArray();
  assert.equal(rows[0].get('s').object.direction, 'rtl');
  assert(marker.has(rows[0].get('s').subject));
  assert(marker.has(result.variables[0]));
  assert.deepEqual(
    await (
      await dataset(async () =>
        Response.json({ head: { vars: ['s'] }, results: { bindings: [] } }),
      ).select('SELECT * WHERE {}')
    ).toArray(),
    [],
  );
});
test('N-Quads handles comments, RDF escapes, direction, Unicode blank labels, nested triple objects and custom factory', async () => {
  const input = stream(
    '# comment\r\n\n_:😀 <urn:p> "line\\n\\t\\b\\f\\r\\\"\\\\\\U0001F600"@en--rtl . # tail\n<urn:s> <urn:p> <<( <urn:a> <urn:b> "é" )>> <urn:g> .',
    { chunk: 1 },
  );
  let named = 0;
  const custom = {
    ...factory,
    namedNode(value) {
      named++;
      return factory.namedNode(value);
    },
  };
  const result = await dataset(
    async () => new Response(input.body, { headers: { 'content-type': 'application/n-quads' } }),
  ).construct('CONSTRUCT {} WHERE {}', { factory: custom });
  const rows = await result.toArray();
  assert.equal(rows.length, 2);
  assert.equal(rows[0].object.direction, 'rtl');
  assert.equal(rows[0].object.value, 'line\n\t\b\f\r"\\😀');
  assert.equal(rows[1].object.termType, 'Quad');
  assert.equal(rows[1].graph.value, 'urn:g');
  assert(named >= 5);
  assert.equal(input.body.locked, false);
});
test('N-Quads rejects malformed syntax and invalid UTF-8 with typed errors', async () => {
  for (const text of [
    '<urn:s><urn:p> "x" .',
    '<urn:s> <urn:p> "unfinished .',
    '<urn:s> <urn:p> "bad\\q" .',
    '<urn:s> <urn:p> "bad\\uD800" .',
    '<relative> <urn:p> "x" .',
    '"s" <urn:p> "x" .',
    '<urn:s> _:p "x" .',
    '<urn:s> <urn:p> "x" "graph" .',
    '<urn:s> <urn:p> "x"@en--bad .',
    '<urn:s> <urn:p> <urn:o> . extra',
  ]) {
    const ds = dataset(
      async () => new Response(text, { headers: { 'content-type': 'application/n-quads' } }),
    );
    await assert.rejects(
      (await ds.construct('CONSTRUCT {} WHERE {}')).toArray(),
      RdfSyntaxError,
      text,
    );
  }
  const input = stream(new Uint8Array([0xc3, 0x28]));
  await assert.rejects(
    (
      await dataset(
        async () =>
          new Response(input.body, { headers: { 'content-type': 'application/n-quads' } }),
      ).construct('CONSTRUCT {} WHERE {}')
    ).toArray(),
    RdfSyntaxError,
  );
});
test('CSRF follows session changes and typed admin shares basic authorization and credentials', async () => {
  const requests = [];
  let sessions = 0;
  const client = new SparklesClient('http://example.test', {
    credentials: 'include',
    basic: { username: 'ü', password: 'secret' },
    fetch: async (input) => {
      const r = new Request(input);
      requests.push(r);
      if (r.url.endsWith('/$/whoami')) return Response.json({ csrfToken: 'session' + ++sessions });
      return Response.json({ inserted: 0, deleted: 0 });
    },
  });
  await client.dataset('wiki').update('INSERT DATA {}');
  await client.api.DELETE('/$/datasets/{name}', { params: { path: { name: 'wiki' } } });
  const writes = requests.filter((r) => r.method !== 'GET');
  assert.equal(writes[0].headers.get('x-sparkles-csrf'), 'session1');
  assert.equal(writes[1].headers.get('x-sparkles-csrf'), 'session2');
  assert(
    requests.every(
      (r) =>
        r.credentials === 'include' &&
        r.headers.get('authorization') === 'Basic ' + Buffer.from('ü:secret').toString('base64'),
    ),
  );
});
test('structured write errors retain validation and request id; ETag failures are conflicts', async () => {
  const ds = dataset(async () =>
    Response.json(
      { error: 'rejected', validation: { conforms: false, count: 1 } },
      { status: 422, headers: { 'x-request-id': 'request' } },
    ),
  );
  await assert.rejects(
    ds.update('INSERT DATA {}'),
    (e) =>
      e instanceof WriteRejectedError &&
      e.details.validation.count === 1 &&
      e.details.requestId === 'request',
  );
  const conflict = dataset(async () => Response.json({ error: 'etag mismatch' }, { status: 412 }));
  await assert.rejects(
    conflict.graphs.put(undefined, '', { contentType: 'text/turtle', ifMatch: 'old' }),
    ConflictError,
  );
});

test('history counts convert safely to bigint and branch identity survives next-page URLs', async () => {
  const ds = dataset(async () =>
    Response.json({
      dataset: 'wiki',
      datasetId: 'id',
      head: 5,
      firstRetained: 0,
      oldestReconstructable: null,
      complete: true,
      commits: [
        {
          seq: 5,
          parent: 4,
          inserted: 1,
          deleted: 0,
          quads: 7,
          parents: [{ branchId: 'b', seq: 4 }],
          mergedFrom: { branchId: 'a', seq: 3 },
        },
      ],
      next: '/$/commits/wiki?before=5&limit=1',
    }),
  );
  const page = await ds.commits({ limit: 1 });
  assert.equal(page.head, 5n);
  assert.equal(page.commits[0].seq, 5n);
  assert.equal(page.commits[0].parents[0].seq, 4n);
  assert.equal(new URL(page.next, 'http://example.test').searchParams.get('branch'), 'work');
  const unsafe = dataset(async () => Response.json({ head: 9007199254740992, commits: [] }));
  await assert.rejects(unsafe.commits(), InvalidInputError);
});
test(
  'graph response bodies and typed admin bodies remain cancellable with injected transports',
  { timeout: 1500 },
  async () => {
    const input = stream('', { hang: true });
    const controller = new AbortController();
    const ds = dataset(async () => new Response(input.body));
    const response = await ds.graphs.get(undefined, { signal: controller.signal });
    const waiting = response.text();
    controller.abort();
    await assert.rejects(waiting, CancelledError);
    assert.equal(input.cancels, 1);
    assert.equal(input.body.locked, false);
  },
);
test(
  'late responses from aborted injected fetches are cancelled when they eventually arrive',
  { timeout: 1500 },
  async () => {
    let deliver;
    const controller = new AbortController();
    const input = stream('', { hang: true });
    const ds = dataset(
      () =>
        new Promise((resolve) => {
          deliver = resolve;
        }),
    );
    const waiting = ds.ask('ASK {}', { signal: controller.signal });
    await new Promise((resolve) => setImmediate(resolve));
    controller.abort();
    await assert.rejects(waiting, CancelledError);
    deliver(new Response(input.body));
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(input.cancels, 1);
  },
);

test(
  'cancellation and timeout interrupt a CSRF provider that never settles before dispatch',
  { timeout: 1500 },
  async () => {
    let sent = 0;
    const client = new SparklesClient('http://example.test', {
      credentials: 'include',
      csrfToken: () => new Promise(() => {}),
      fetch: async () => {
        sent++;
        return ask();
      },
    });
    const controller = new AbortController();
    const waiting = client.dataset('wiki').update('INSERT DATA {}', { signal: controller.signal });
    setTimeout(() => controller.abort(), 10);
    await assert.rejects(waiting, CancelledError);
    const hold = setTimeout(() => {}, 1000);
    try {
      await assert.rejects(
        client.dataset('wiki').update('INSERT DATA {}', { timeout: 10 }),
        QueryTimeoutError,
      );
    } finally {
      clearTimeout(hold);
    }
    assert.equal(sent, 0);
  },
);

test('noCache reaches Sparkles while unsupported union settings and plain endpoint cache extensions fail before dispatch', async () => {
  let url;
  const ds = dataset(async (input) => {
    url = new URL(input.url);
    return ask();
  });
  await ds.ask('ASK {}', { noCache: true });
  assert.equal(url.searchParams.get('nocache'), 'true');
  await assert.rejects(ds.ask('ASK {}', { unionDefaultGraph: false }), UnsupportedError);
  const plain = SparklesClient.endpoint('http://example.test/query', { fetch: async () => ask() });
  await assert.rejects(plain.ask('ASK {}', { noCache: true }), UnsupportedError);
});
test(
  'aborting while an error body is being read preserves typed cancellation',
  { timeout: 1500 },
  async () => {
    const input = stream('{', { hang: true });
    const controller = new AbortController();
    const waiting = dataset(async () => new Response(input.body, { status: 400 })).update(
      'INSERT DATA {}',
      { signal: controller.signal },
    );
    setTimeout(() => controller.abort(), 10);
    await assert.rejects(waiting, CancelledError);
    assert.equal(input.cancels, 1);
  },
);
test('valid RDFJS factories without an optional variable constructor still produce SELECT metadata', async () => {
  const custom = { ...factory, variable: undefined };
  const result = await dataset(async () =>
    Response.json({ head: { vars: ['s'] }, results: { bindings: [binding] } }),
  ).select('SELECT * WHERE {}', { factory: custom });
  assert.equal(result.variables[0].value, 's');
  assert.equal((await result.toArray())[0].get('s').value, 'urn:s');
});

test('Graph Store accepts streamed RDF request bodies in Node Fetch', async () => {
  let text;
  const ds = dataset(async (request) => {
    text = await request.text();
    return Response.json({
      datasetId: 'id',
      committed: true,
      commit: { seq: 1, kind: 'gsp-post' },
    });
  });
  const body = new ReadableStream({
    start(c) {
      c.enqueue(new TextEncoder().encode('<urn:s> <urn:p> "streamed" .'));
      c.close();
    },
  });
  const receipt = await ds.graphs.post(undefined, body, { contentType: 'application/n-triples' });
  assert.equal(receipt.commit.seq, 1n);
  assert.equal(text, '<urn:s> <urn:p> "streamed" .');
});
