import test, { after } from 'node:test';
// Keep the test process alive while native Tokio work has no JS timer handles.
const keepAlive = setInterval(() => {}, 1000);
after(() => clearInterval(keepAlive));
import assert from 'node:assert/strict';
import { Dataset, CancelledError, InvalidInputError } from '../dist/index.js';

test(
  'explicit streaming queries pin snapshots and expose unknown size',
  { timeout: 30000 },
  async () => {
    const ds = Dataset.memory();
    try {
      await ds.load('<urn:s1> <urn:p> 1 . <urn:s2> <urn:p> 2 .', { format: 'ttl' });
      const q = 'SELECT ?s ?o WHERE { ?s <urn:p> ?o }';
      const result = await ds.select(q, {
        execution: 'streaming',
        batchSize: 1,
        allowMaterialization: false,
      });
      assert.equal(result.size, undefined);
      assert.equal((await result.stats()).stats.rowsProduced, 0);
      await ds.update('INSERT DATA { <urn:s3> <urn:p> 3 }');
      assert.equal((await result.toArray()).length, 2);
      const final = await result.stats();
      assert.equal(final.stats.status, 'complete');
      const body = await ds.queryToStream(q, { execution: 'streaming', batchSize: 1 });
      assert.equal((await new Response(body).json()).results.bindings.length, 3);
    } finally {
      await ds.close();
    }
  },
);

test(
  'streaming early return and transaction rejection release resources',
  { timeout: 30000 },
  async () => {
    const ds = Dataset.memory();
    try {
      const result = await ds.select('SELECT ?x { VALUES ?x { 1 2 3 } }', {
        execution: 'streaming',
        batchSize: 1,
      });
      await result.next();
      await result.return();
      assert.equal((await result.stats()).stats.status, 'stopped');
      await ds.transaction(async (tx) => {
        await assert.rejects(
          tx.select('SELECT * WHERE {}', { execution: 'streaming' }),
          InvalidInputError,
        );
      });
      const controller = new AbortController();
      const cancelled = await ds.select('SELECT ?x { VALUES ?x { 1 2 3 } }', {
        execution: 'streaming',
        batchSize: 1,
        signal: controller.signal,
      });
      controller.abort();
      await assert.rejects(
        cancelled.next(),
        (e) => e.name === 'AbortError' || e instanceof CancelledError,
      );
      await cancelled.close();
    } finally {
      await ds.close();
    }
  },
);

test(
  'graph and ASK cursors preserve query forms and byte streaming',
  { timeout: 30000 },
  async () => {
    const ds = Dataset.memory();
    try {
      await ds.load('<urn:s1> <urn:p> 1 . <urn:s2> <urn:p> 2 .', { format: 'ttl' });
      const q = 'CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }';
      const graph = await ds.construct(q, {
        execution: 'streaming',
        batchSize: 1,
        allowMaterialization: false,
      });
      await ds.update('INSERT DATA { <urn:s3> <urn:p> 3 }');
      assert.equal((await graph.toArray()).length, 2);
      assert.equal((await graph.stats()).stats.status, 'complete');
      assert.equal(await ds.ask('ASK { ?s <urn:p> ?o }', { execution: 'streaming' }), true);
      assert.equal(await ds.ask('ASK { ?s <urn:absent> ?o }', { execution: 'streaming' }), false);
      const body = await ds.queryToStream(q, {
        execution: 'streaming',
        accept: 'application/n-quads',
      });
      assert.equal((await new Response(body).text()).trim().split('\n').length, 3);
    } finally {
      await ds.close();
    }
  },
);

test('automatic small queries retain collected result semantics', { timeout: 30000 }, async () => {
  const ds = Dataset.memory();
  try {
    const result = await ds.select('SELECT ?x { VALUES ?x { 1 2 3 } }', { execution: 'auto' });
    assert.equal(result.size, 3);
    assert.equal((await result.toArray()).length, 3);
    assert.equal(await ds.ask('ASK {}', { execution: 'auto' }), true);
  } finally {
    await ds.close();
  }
});
