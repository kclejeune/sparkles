import nodeTest from 'node:test';
const test = (name, options, callback) => {
  if (typeof options === 'function') {
    callback = options;
    options = {};
  }
  return nodeTest(name, options, async (t) => {
    const resources = [];
    t.resource = (value) => {
      resources.push(value);
      return value;
    };
    t.cleanup = async (t) => {
      for (const value of resources.splice(0).reverse()) await value.close();
    };
    try {
      await callback(t);
    } finally {
      await t.cleanup();
    }
  });
};
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Worker } from 'node:worker_threads';
import { Readable } from 'node:stream';
import {
  Dataset,
  factory,
  ConflictError,
  InvalidInputError,
  QueryTimeoutError,
  SparqlSyntaxError,
  UnsupportedError,
} from '../dist/index.js';

assert(Number(process.versions.node.split('.')[0]) >= 22);
const quad = (n, value = 'x') =>
  factory.quad(factory.namedNode(`urn:s${n}`), factory.namedNode('urn:p'), factory.literal(value));
test('unsupported transaction update controls roll back without publication', async (t) => {
  const ds = t.resource(Dataset.memory());
  const head = (await ds.info()).head.seq;
  for (const options of [{ dryRun: true }, { dryRun: { changes: 10 } }, { ifHead: 999n }]) {
    await assert.rejects(
      ds.transaction(async (tx) => {
        await tx.add(quad(1));
        await tx.update('INSERT DATA {<urn:t> <urn:p> 2}', options);
      }),
      UnsupportedError,
    );
    assert.equal(await ds.count(), 0n);
    assert.equal((await ds.info()).head.seq, head);
  }
  await ds.transaction(async (tx) =>
    tx.update('INSERT DATA {<urn:t> <urn:p> 2}', { dryRun: false }),
  );
  assert.equal(await ds.count(), 1n);
});
test('interrupted loads cancel and release stalled Web readers', { timeout: 5000 }, async (t) => {
  for (const mode of ['abort', 'timeout', 'close']) {
    const ds = t.resource(Dataset.memory());
    let cancellations = 0;
    let chunks = 0;
    let stalled;
    const reading = new Promise((resolve) => {
      stalled = resolve;
    });
    const input = new ReadableStream({
      pull(controller) {
        if (chunks++ === 0) controller.enqueue(new TextEncoder().encode('<urn:s> <urn:p> "x" .'));
        else {
          stalled();
          return new Promise(() => {});
        }
      },
      cancel() {
        cancellations++;
        return new Promise(() => {});
      },
    });
    const controller = new AbortController();
    const loading = ds.load(input, {
      format: 'nt',
      signal: mode === 'abort' ? controller.signal : undefined,
      timeout: mode === 'timeout' ? 30 : undefined,
    });
    const rejected = assert.rejects(
      loading,
      mode === 'timeout' ? QueryTimeoutError : /stop|closing/,
    );
    await reading;
    if (mode === 'abort') controller.abort(new Error('stop'));
    else if (mode === 'close') await ds.close();
    await rejected;
    assert.equal(input.locked, false, mode);
    assert.equal(cancellations, 1, mode);
    if (mode !== 'close') {
      assert.equal(await ds.count(), 0n);
      assert.equal((await ds.info()).head.seq, 0n);
    }
  }
});
test('memory queries, receipts, foreign terms, retained batches and early break', async (t) => {
  const ds = t.resource(Dataset.memory());
  assert.equal(await ds.count(), 0n);
  assert.equal((await ds.addAll(Array.from({ length: 30 }, (_, i) => quad(i)))).inserted, 30n);
  assert(await ds.has(quad(2)));
  const r = await ds.select('SELECT ?s ?o WHERE {?s <urn:p> ?o} ORDER BY ?s', { batchSize: 3 });
  const first = (await r.next()).value;
  for await (const _ of r) break;
  assert.equal(first.get('o').value, 'x');
  assert.equal(first.toObject().o.value, 'x');
  assert.equal((await (await ds.select('SELECT * WHERE {?s ?p ?o}')).toArray()).length, 30);
  await assert.rejects(ds.select('ASK {}'), InvalidInputError);
  await assert.rejects(ds.query('SELECT * WHERE {'), SparqlSyntaxError);
});
test('callback reads own state, rejects self-writes, rolls back and escaped tx rejects', async (t) => {
  const ds = t.resource(Dataset.memory());
  let escaped;
  const result = await ds.transaction(
    async (tx) => {
      escaped = tx;
      await tx.add(quad(1));
      assert.equal((await tx.match().toArray()).length, 1);
      assert.equal(await ds.count(), 0n);
      await assert.rejects(ds.add(quad(2)), ConflictError);
      return 42;
    },
    { message: 'seed' },
  );
  assert.equal(result.value, 42);
  assert.equal(result.receipt.commit.seq, 1n);
  assert.equal(result.receipt.commit.message, 'seed');
  await assert.rejects(escaped.add(quad(3)), InvalidInputError);
  await assert.rejects(
    ds.transaction(async (tx) => {
      await tx.add(quad(4));
      throw new Error('rollback');
    }),
    /rollback/,
  );
  assert.equal(await ds.count(), 1n);
});
test(
  'timeout releases a transaction whose callback never settles',
  { timeout: 5000 },
  async (t) => {
    const ds = t.resource(Dataset.memory());
    await assert.rejects(
      ds.transaction(
        async (tx) => {
          await tx.add(quad(1));
          await new Promise(() => {});
        },
        { timeout: 30 },
      ),
      QueryTimeoutError,
    );
    assert.equal(await ds.count(), 0n);
    assert(await ds.add(quad(2)));
  },
);
test('queued abort/noWait and unrelated writes preserve FIFO', { timeout: 5000 }, async (t) => {
  const ds = t.resource(Dataset.memory());
  const tx = await ds.beginTransaction();
  const abort = new AbortController();
  const wait = ds.update('INSERT DATA { <urn:x> <urn:p> 1 }', { signal: abort.signal });
  abort.abort(new Error('stop'));
  await assert.rejects(wait, /stop/);
  await assert.rejects(
    ds.update('INSERT DATA { <urn:y> <urn:p> 1 }', { noWait: true }),
    ConflictError,
  );
  const other = ds.add(quad(3));
  await tx.rollback();
  assert(await other);
});
test('stream inputs/dumps and path loads round trip; malformed loads are atomic', async (t) => {
  const ds = t.resource(Dataset.memory());
  await ds.load(Readable.from(['<urn:s> <urn:p> "', 'héllo" .']), { format: 'nt' });
  await ds.load(
    new ReadableStream({
      start(c) {
        c.enqueue(new TextEncoder().encode('<urn:b> <urn:p> "x" .'));
        c.close();
      },
    }),
    { format: 'nt' },
  );
  assert.equal(await ds.count(), 2n);
  await assert.rejects(ds.load('<urn:c> <urn:p> "x" .\nBROKEN', { format: 'nt' }));
  assert.equal(await ds.count(), 2n);
  const copy = t.resource(Dataset.memory());
  await copy.load(ds.dump({ format: 'nq', compression: 'gzip' }), {
    format: 'nq',
    compression: 'gzip',
  });
  assert.equal(await copy.count(), 2n);
  const stream = await ds.queryToStream('SELECT * WHERE {?s ?p ?o}');
  const body = await new Response(stream).json();
  assert.equal(body.results.bindings.length, 2);
  const temp = await mkdtemp(join(tmpdir(), 'sparkles-node-files-'));
  try {
    const file = join(temp, 'x.nt');
    await writeFile(file, '<urn:f> <urn:p> "file" .');
    await ds.load({ path: file });
    assert.equal(await ds.count(), 3n);
    const dump = join(temp, 'dump.nq.gz');
    await ds.dumpToFile(dump);
    const other = t.resource(Dataset.memory());
    await other.load({ path: dump });
    assert.equal(await other.count(), 3n);
  } finally {
    await t.cleanup();
    await rm(temp, { recursive: true, force: true });
  }
});
test(
  'persistent registry shares Rust stores across worker environments and clean shutdown',
  { timeout: 15000 },
  async (t) => {
    const path = await mkdtemp(join(tmpdir(), 'sparkles-node-worker-'));
    try {
      const ds = t.resource(await Dataset.open(path));
      await ds.add(quad(1));
      const alias = t.resource(await Dataset.open(path));
      assert.equal(alias.id, ds.id);
      assert.equal(await alias.count(), 1n);
      for (let i = 0; i < 3; i++) {
        const worker = new Worker(new URL('./worker.mjs', import.meta.url), {
          workerData: { path },
        });
        const result = await new Promise((resolve, reject) => {
          worker.once('message', resolve);
          worker.once('error', reject);
          worker.once('exit', (code) => {
            if (code) reject(new Error(`worker exit ${code}`));
          });
        });
        assert.equal(result.id, ds.id);
        assert.equal(result.count, String(i + 2));
        await new Promise((resolve, reject) =>
          worker.once('exit', (code) =>
            code ? reject(new Error(`worker exit ${code}`)) : resolve(),
          ),
        );
      }
      assert.equal(await ds.count(), 4n);
    } finally {
      await t.cleanup();
      await rm(path, { recursive: true, force: true });
    }
  },
);
test('read-only handles reject every write and closed handles reject operations', async (t) => {
  const ds = Dataset.memory({ readOnly: true });
  await assert.rejects(ds.add(quad(1)));
  await assert.rejects(ds.load('<urn:s> <urn:p> 1 .', { format: 'ttl' }));
  await assert.rejects(ds.beginTransaction());
  await ds.close();
  await assert.rejects(ds.count(), InvalidInputError);
});
test('result abort and dataset close release SELECT, CONSTRUCT and match cursors', async (t) => {
  const ds = Dataset.memory();
  await ds.addAll([quad(1), quad(2), quad(3)]);
  const signal = new AbortController();
  const rows = await ds.select('SELECT * WHERE {?s ?p ?o}', {
    signal: signal.signal,
    batchSize: 1,
  });
  await rows.next();
  signal.abort(new Error('consumption stop'));
  await assert.rejects(rows.next(), /consumption stop/);
  const select = await ds.select('SELECT * WHERE {?s ?p ?o}');
  const construct = await ds.construct('CONSTRUCT {?s ?p ?o} WHERE {?s ?p ?o}');
  const match = ds.match()[Symbol.asyncIterator]();
  await match.next();
  await ds.close();
  assert.equal((await select.next()).done, true);
  assert.equal((await construct.next()).done, true);
  assert.equal((await match.next()).done, true);
});
test(
  'stalled input producers are cancelled and close releases their transaction',
  { timeout: 5000 },
  async (t) => {
    const ds = Dataset.memory();
    const input = {
      async *[Symbol.asyncIterator]() {
        yield new TextEncoder().encode('<urn:s> <urn:p> "x" .');
        await new Promise(() => {});
      },
    };
    const signal = new AbortController();
    const load = ds.load(input, { format: 'nt', signal: signal.signal });
    await new Promise((r) => setTimeout(r, 30));
    signal.abort(new Error('producer stop'));
    await assert.rejects(load, /producer stop/);
    assert.equal(await ds.count(), 0n);
    assert(await ds.add(quad(1)));
    const closing = ds.load(input, { format: 'nt' });
    const handled = assert.rejects(closing, /closing/);
    await new Promise((r) => setTimeout(r, 30));
    await ds.close();
    await handled;
  },
);
test('invalid native batches poison transactions without partial publication', async (t) => {
  const ds = t.resource(Dataset.memory());
  const tx = await ds.beginTransaction();
  await tx.add(quad(10));
  const bad = {
    termType: 'Quad',
    value: '',
    subject: factory.literal('bad'),
    predicate: factory.namedNode('urn:p'),
    object: factory.literal('x'),
    graph: factory.defaultGraph(),
  };
  await assert.rejects(tx.addBatch([quad(11), bad]));
  await assert.rejects(tx.commit());
  assert.equal(await ds.count(), 0n);
});
test(
  'abrupt worker teardown releases transaction and stalled upload owners',
  { timeout: 15000 },
  async (t) => {
    const path = await mkdtemp(join(tmpdir(), 'sparkles-node-abrupt-'));
    try {
      const ds = t.resource(await Dataset.open(path));
      for (const mode of ['transaction', 'upload']) {
        const worker = new Worker(new URL('./abrupt-worker.mjs', import.meta.url), {
          workerData: { path, mode },
        });
        await new Promise((resolve, reject) => {
          worker.once('message', resolve);
          worker.once('error', reject);
        });
        await worker.terminate();
        await ds.update('INSERT DATA { <urn:released> <urn:p> "' + mode + '" }', { timeout: 2000 });
      }
      assert.equal(await ds.count(), 2n);
    } finally {
      await t.cleanup();
      await rm(path, { recursive: true, force: true });
    }
  },
);
test(
  'abrupt worker termination safely releases pending native stream promises',
  { timeout: 10000 },
  async (t) => {
    const path = await mkdtemp(join(tmpdir(), 'sparkles-stream-owner-'));
    try {
      const ds = t.resource(await Dataset.open(path));
      for (const mode of [
        'uploadPush',
        'uploadFinish',
        'dumpNext',
        'waitCommit',
        'beginQueued',
        'loadQueued',
        'txApply',
        'txEnd',
      ]) {
        const held = mode.endsWith('Queued') ? await ds.beginTransaction() : undefined;
        const worker = new Worker(new URL('./streams-worker.mjs', import.meta.url), {
          workerData: { path, mode },
        });
        await new Promise((resolve, reject) => {
          worker.once('message', resolve);
          worker.once('error', reject);
          worker.once('exit', (code) => {
            if (code !== 0) reject(new Error(`worker exit ${code}`));
          });
        });
        await worker.terminate();
        await held?.rollback();
        await ds.update(`INSERT DATA {<urn:${mode}> <urn:p> 1}`, { timeout: 1000 });
      }
      const rows = await ds.select('SELECT ?s WHERE {?s <urn:p> 1}');
      assert.equal((await rows.toArray()).length, 8);
    } finally {
      await t.cleanup();
      await rm(path, { recursive: true, force: true });
    }
  },
);
