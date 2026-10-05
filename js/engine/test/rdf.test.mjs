import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Readable } from 'node:stream';
import { Worker } from 'node:worker_threads';
import { setTimeout as delay } from 'node:timers/promises';
import {
  parse,
  serialize,
  factory,
  InvalidInputError,
  RdfSyntaxError,
  QueryTimeoutError,
} from '../dist/index.js';

test(
  'a completed quad is observable before its producer reaches EOF',
  { timeout: 5000 },
  async () => {
    const source = (first) => ({
      [Symbol.asyncIterator]() {
        let sent = false;
        return {
          next() {
            if (!sent) {
              sent = true;
              return Promise.resolve({ done: false, value: first });
            }
            return new Promise(() => {});
          },
          return: async () => ({ done: true }),
        };
      },
    });
    const parsed = parse(source('<urn:s> <urn:p> "snow ☃" <urn:g> .\n'));
    try {
      assert.equal((await parsed.next()).value.object.value, 'snow ☃');
    } finally {
      await parsed.return();
    }
    const quad = factory.quad(
      factory.namedNode('urn:s1'),
      factory.namedNode('urn:p'),
      factory.literal('x'),
    );
    const serialized = serialize(source(quad)).getReader();
    try {
      assert.match(new TextDecoder().decode((await serialized.read()).value), /urn:s1/);
    } finally {
      await serialized.cancel();
    }
  },
);

const q = (n) =>
  factory.quad(
    factory.namedNode(`urn:s${n}`),
    factory.namedNode('urn:p'),
    factory.literal(`value ${n} ☃`),
  );
const nq = '<urn:s> <urn:p> "snow ☃" <urn:g> .\n';
const all = async (input) => {
  const rows = [];
  for await (const row of input) rows.push(row);
  return rows;
};
const bytes = async (stream) => new Uint8Array(await new Response(stream).arrayBuffer());

test('standalone RDF parsing accepts text, bytes, paths, Node/Web streams and async chunks', async () => {
  const encoded = new TextEncoder().encode(nq);
  const dir = await mkdtemp(join(tmpdir(), 'sparkles-rdf-'));
  try {
    const path = join(dir, 'data.nq');
    await writeFile(path, encoded);
    const inputs = [
      nq,
      encoded,
      { path },
      Readable.from([encoded]),
      new ReadableStream({
        start(c) {
          c.enqueue(encoded);
          c.close();
        },
      }),
      {
        async *[Symbol.asyncIterator]() {
          for (const byte of encoded) yield Uint8Array.of(byte);
        },
      },
      {
        async *[Symbol.asyncIterator]() {
          yield nq.slice(0, 8);
          yield nq.slice(8);
        },
      },
    ];
    for (const input of inputs) {
      const [row] = await all(parse(input));
      assert.equal(row.subject.value, 'urn:s');
      assert.equal(row.object.value, 'snow ☃');
      assert.equal(row.graph.value, 'urn:g');
    }
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('RDF syntax, base resolution, custom factories and format errors are preserved', async () => {
  let made = 0;
  const custom = {
    ...factory,
    quad(...args) {
      made++;
      return factory.quad(...args);
    },
  };
  const rows = await all(
    parse('<s> <p> "x" .', { format: 'ttl', baseIri: 'https://example.org/', factory: custom }),
  );
  assert.equal(rows[0].subject.value, 'https://example.org/s');
  assert.equal(made, 1);
  await assert.rejects(all(parse('<urn:s> <urn:p> "unterminated')), RdfSyntaxError);
  await assert.rejects(all(parse(nq, { format: 'unknown' })), InvalidInputError);
  await assert.rejects(all(parse(nq, { baseIri: 'not-an-iri' })), InvalidInputError);
});

test('standalone serialization round trips supported RDF syntaxes and codecs', async () => {
  for (const format of ['nq', 'nt', 'ttl', 'trig', 'rdfxml', 'jsonld']) {
    const output = await bytes(serialize([q(1), q(2)], { format }));
    assert.equal((await all(parse(output, { format }))).length, 2, format);
  }
  for (const compression of ['gzip', 'zstd', 'brotli']) {
    const output = await bytes(serialize([q(1)], { compression }));
    assert.equal((await all(parse(output, { compression }))).length, 1, compression);
  }
});

test('RDF serializer preserves dataset graphs, blank nodes, directional and triple terms', async () => {
  const inner = factory.quad(
    factory.namedNode('urn:a'),
    factory.namedNode('urn:b'),
    factory.literal('v'),
  );
  const rows = [
    factory.quad(
      factory.blankNode('subject'),
      factory.namedNode('urn:p'),
      inner,
      factory.namedNode('urn:g'),
    ),
    factory.quad(
      factory.namedNode('urn:other'),
      factory.namedNode('urn:p'),
      factory.literal('x', { language: 'en', direction: 'ltr' }),
    ),
  ];
  const parsed = await all(parse(await bytes(serialize(rows))));
  assert.equal(parsed[0].graph.value, 'urn:g');
  assert.equal(parsed[0].object.termType, 'Quad');
  assert.equal(parsed[1].object.direction, 'ltr');
});

test(
  'invalid serializer input and upstream failures terminate instead of hanging',
  { timeout: 5000 },
  async () => {
    await assert.rejects(
      bytes(serialize([{ termType: 'NamedNode', value: 'urn:bad' }])),
      InvalidInputError,
    );
    const malformed = { ...q(1), predicate: factory.literal('bad') };
    await assert.rejects(bytes(serialize([malformed])), InvalidInputError);
    const failure = new Error('upstream failed');
    const source = {
      async *[Symbol.asyncIterator]() {
        throw failure;
      },
    };
    await assert.rejects(bytes(serialize(source)), (e) => e === failure);
    await assert.rejects(all(parse(source)), (e) => e === failure);
  },
);

test(
  'parser iterator.return releases a pending read and calls stalled producer cleanup',
  { timeout: 5000 },
  async () => {
    let returned = 0;
    const source = {
      [Symbol.asyncIterator]() {
        return {
          next: () => new Promise(() => {}),
          return() {
            returned++;
            return new Promise(() => {});
          },
        };
      },
    };
    const reader = parse(source);
    const pending = reader.next();
    await delay(20);
    assert.equal((await reader.return()).done, true);
    assert.equal((await pending).done, true);
    assert.equal(returned, 1);
  },
);

test(
  'parse and serialize deadlines interrupt stalled input without waiting for producer',
  { timeout: 5000 },
  async () => {
    let cleaned = 0;
    const source = {
      [Symbol.asyncIterator]() {
        return {
          next: () => new Promise(() => {}),
          return() {
            cleaned++;
            return new Promise(() => {});
          },
        };
      },
    };
    await assert.rejects(all(parse(source, { timeout: 30 })), QueryTimeoutError);
    await assert.rejects(bytes(serialize(source, { timeout: 30 })), QueryTimeoutError);
    assert.equal(cleaned, 2);
  },
);

test(
  'serialize cancellation releases pending reads and never-settling producer',
  { timeout: 5000 },
  async () => {
    let returned = 0;
    const source = {
      [Symbol.asyncIterator]() {
        return {
          next: () => new Promise(() => {}),
          return() {
            returned++;
            return new Promise(() => {});
          },
        };
      },
    };
    const reader = serialize(source).getReader();
    const pending = reader.read();
    await delay(20);
    await reader.cancel();
    assert.equal((await pending).done, true);
    assert.equal(returned, 1);
  },
);

test(
  'AbortSignal propagates its reason and releases both RDF stream directions',
  { timeout: 5000 },
  async () => {
    const source = {
      [Symbol.asyncIterator]() {
        return { next: () => new Promise(() => {}), return: async () => ({ done: true }) };
      },
    };
    for (const read of [
      () => all(parse(source, { signal: ctl.signal })),
      () => bytes(serialize(source, { signal: ctl.signal })),
    ]) {
      var ctl = new AbortController();
      const reason = new Error('stop RDF');
      const pending = read();
      await delay(20);
      ctl.abort(reason);
      await assert.rejects(pending, (e) => e === reason);
    }
  },
);

test('early parser break cancels an owned Web stream and releases its reader lock', async () => {
  let cancelled = 0;
  const stream = new ReadableStream({
    pull(c) {
      c.enqueue(new TextEncoder().encode(nq));
    },
    cancel() {
      cancelled++;
    },
  });
  for await (const _ of parse(stream)) break;
  assert.equal(cancelled, 1);
  assert.equal(stream.locked, false);
});

test(
  'serialization backpressure bounds producer advancement without materializing its input',
  { timeout: 5000 },
  async () => {
    let produced = 0,
      returned = 0;
    const source = {
      [Symbol.asyncIterator]() {
        return {
          async next() {
            return { value: q(++produced), done: false };
          },
          async return() {
            returned++;
            return { done: true };
          },
        };
      },
    };
    const stream = serialize(source);
    await delay(50);
    assert(produced > 0);
    assert(produced < 20, `advanced ${produced} quads without a consumer`);
    await stream.cancel();
    assert.equal(returned, 1);
  },
);

test('RDF stream options reject invalid budgets and producer chunk types', async () => {
  for (const timeout of [0, -1, NaN, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
    await assert.rejects(all(parse(nq, { timeout })), InvalidInputError);
    assert.throws(() => serialize([], { timeout }), InvalidInputError);
  }
  await assert.rejects(
    all(
      parse({
        async *[Symbol.asyncIterator]() {
          yield 123;
        },
      }),
    ),
    InvalidInputError,
  );
});

test('empty synchronous chunks cannot starve the parser timeout', { timeout: 5000 }, async () => {
  let returned = 0;
  const source = {
    [Symbol.asyncIterator]() {
      return {
        next: () => ({ done: false, value: '' }),
        return() {
          returned++;
          return { done: true };
        },
      };
    },
  };
  await assert.rejects(all(parse(source, { timeout: 20 })), QueryTimeoutError);
  assert.equal(returned, 1);
});

test('pre-aborted RDF operations preserve the caller cancellation reason', async () => {
  const ctl = new AbortController();
  const reason = new Error('already stopped');
  ctl.abort(reason);
  await assert.rejects(all(parse(nq, { signal: ctl.signal })), (e) => e === reason);
  assert.throws(
    () => serialize([], { signal: ctl.signal }),
    (e) => e === reason,
  );
});

test(
  'abrupt Worker teardown releases RDF workers stalled in either direction',
  { timeout: 5000 },
  async () => {
    for (const mode of ['parse', 'serialize']) {
      const worker = new Worker(
        `const {parentPort,workerData}=require('node:worker_threads');
      (async()=>{const api=await import(workerData.url);const source={
        [Symbol.asyncIterator](){return {next:()=>new Promise(()=>{}),return:()=>new Promise(()=>{})}}};
        if(workerData.mode==='parse')void api.parse(source).next();else void api.serialize(source).getReader().read();
        parentPort.postMessage('ready');})();`,
        {
          eval: true,
          workerData: { url: new URL('../dist/index.js', import.meta.url).href, mode },
        },
      );
      await new Promise((resolve, reject) => {
        worker.once('message', resolve);
        worker.once('error', reject);
      });
      await worker.terminate();
    }
    assert.equal((await all(parse(nq))).length, 1);
  },
);
