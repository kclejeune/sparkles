import test, { after } from 'node:test';
// Keep the test process alive while native Tokio work has no JS timer handles.
const keepAlive = setInterval(() => {}, 1000);
after(() => clearInterval(keepAlive));
import assert from 'node:assert/strict';
import { availableParallelism } from 'node:os';
import {
  Dataset,
  CancelledError,
  ConflictError,
  InvalidInputError,
  configure,
} from '../dist/index.js';

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

test('idle streaming cursors do not hold query permits', { timeout: 30000 }, async () => {
  const ds = Dataset.memory();
  configure({ maxConcurrentQueries: 1 });
  try {
    await ds.load('<urn:s1> <urn:p> 1 . <urn:s2> <urn:p> 2 . <urn:s3> <urn:p> 3 .', {
      format: 'ttl',
    });
    const q = 'SELECT ?s WHERE { ?s <urn:p> ?o }';
    const cursors = [];
    for (let i = 0; i < 4; i++) {
      const cursor = await ds.select(q, { execution: 'streaming', batchSize: 1, timeout: 5000 });
      await cursor.next();
      cursors.push(cursor);
    }
    // With one permit, this query runs only if the idle cursors gave theirs back.
    assert.equal((await ds.select(q, { timeout: 5000 })).size, 3);
    for (const cursor of cursors) assert.equal((await cursor.toArray()).length, 2);
  } finally {
    configure({ maxConcurrentQueries: availableParallelism() });
    await ds.close();
  }
});

test(
  'maxConcurrentQueries is validated and limits running queries',
  { timeout: 30000 },
  async () => {
    assert.throws(() => configure({ maxConcurrentQueries: 0 }), InvalidInputError);
    assert.throws(() => configure({ maxConcurrentQueries: 1.5 }), InvalidInputError);
    const ds = Dataset.memory();
    configure({ maxConcurrentQueries: 1 });
    try {
      const data = Array.from({ length: 10000 }, (_, i) => `<urn:s${i}> <urn:p> ${i} .`).join('\n');
      await ds.load(data, { format: 'ttl' });
      // A join of 10^8 rows holds the only permit until its timeout.
      const slow = ds
        .select('SELECT (COUNT(*) AS ?n) { ?a <urn:p> ?x . ?b <urn:p> ?y FILTER(?x + ?y < 0) }', {
          timeout: 5000,
        })
        .catch((e) => e);
      await new Promise((resolve) => setTimeout(resolve, 200));
      await assert.rejects(ds.select('SELECT * { ?s ?p ?o }', { noWait: true }), ConflictError);
      await slow;
    } finally {
      configure({ maxConcurrentQueries: availableParallelism() });
      await ds.close();
    }
  },
);

test('existing cursors use the query limit after reconfiguration', { timeout: 30000 }, async () => {
  const ds = Dataset.memory();
  const controller = new AbortController();
  let cursor;
  let slow;
  let body;
  let serialized;
  configure({ maxConcurrentQueries: 1 });
  try {
    const data = Array.from({ length: 10000 }, (_, i) => `<urn:s${i}> <urn:p> ${i} .`).join('\n');
    await ds.load(data, { format: 'ttl' });
    cursor = await ds.select('SELECT ?s { ?s <urn:p> ?o }', {
      execution: 'streaming',
      batchSize: 1,
    });
    // Enough output to fill the native channel and release the stream's permit.
    body = await ds.queryToStream('SELECT ?s { ?s <urn:p> ?o }', {
      execution: 'streaming',
      batchSize: 1,
    });
    configure({ maxConcurrentQueries: 1 });
    slow = ds
      .select('SELECT (COUNT(*) AS ?n) { ?a <urn:p> ?x . ?b <urn:p> ?y FILTER(?x + ?y < 0) }', {
        timeout: 10000,
        signal: controller.signal,
      })
      .catch((e) => e);
    await new Promise((resolve) => setTimeout(resolve, 200));
    await assert.rejects(ds.select('SELECT * { ?s ?p ?o }', { noWait: true }), ConflictError);
    let delivered = false;
    const next = cursor.next().then((row) => {
      delivered = true;
      return row;
    });
    let streamDelivered = false;
    serialized = new Response(body).json().then((result) => {
      streamDelivered = true;
      return result;
    });
    await new Promise((resolve) => setTimeout(resolve, 100));
    assert.equal(delivered, false, 'the old cursor bypassed the occupied query limit');
    assert.equal(streamDelivered, false, 'the old byte stream bypassed the occupied query limit');
    // Raising the limit admits pulls that are already queued.
    configure({ maxConcurrentQueries: 2 });
    assert.equal((await next).done, false);
    assert.equal((await serialized).results.bindings.length, 10000);
    controller.abort();
    await slow;
  } finally {
    controller.abort();
    await slow;
    if (serialized) await serialized;
    else await body?.cancel();
    await cursor?.close();
    configure({ maxConcurrentQueries: availableParallelism() });
    await ds.close();
  }
});

test('a lowered query limit is not exceeded by running queries', { timeout: 30000 }, async () => {
  const ds = Dataset.memory();
  const controllers = [new AbortController(), new AbortController()];
  let slow = [];
  configure({ maxConcurrentQueries: 2 });
  try {
    const data = Array.from({ length: 10000 }, (_, i) => `<urn:s${i}> <urn:p> ${i} .`).join('\n');
    await ds.load(data, { format: 'ttl' });
    slow = controllers.map((controller) =>
      ds
        .select('SELECT (COUNT(*) AS ?n) { ?a <urn:p> ?x . ?b <urn:p> ?y FILTER(?x + ?y < 0) }', {
          timeout: 10000,
          signal: controller.signal,
        })
        .catch((e) => e),
    );
    await new Promise((resolve) => setTimeout(resolve, 200));
    // Both running queries keep their permits, so the lowered limit owes one.
    configure({ maxConcurrentQueries: 1 });
    await assert.rejects(ds.select('SELECT * { ?s ?p ?o }', { noWait: true }), ConflictError);
    controllers[0].abort();
    await slow[0];
    // The first finished query pays the debt instead of admitting a second query.
    await assert.rejects(ds.select('SELECT * { ?s ?p ?o }', { noWait: true }), ConflictError);
    controllers[1].abort();
    await slow[1];
    const r = await ds.select('SELECT * { ?s ?p ?o } LIMIT 1', { noWait: true });
    assert.equal((await r.toArray()).length, 1);
  } finally {
    for (const controller of controllers) controller.abort();
    await Promise.all(slow);
    configure({ maxConcurrentQueries: availableParallelism() });
    await ds.close();
  }
});

test(
  'the next batch is requested once half of the current one is consumed',
  { timeout: 30000 },
  async () => {
    const ds = Dataset.memory();
    try {
      const data = Array.from({ length: 20 }, (_, i) => `<urn:s${i}> <urn:p> ${i} .`).join('\n');
      await ds.load(data, { format: 'ttl' });
      const result = await ds.select('SELECT ?s WHERE { ?s <urn:p> ?o }', {
        execution: 'streaming',
        batchSize: 4,
      });
      await result.next();
      assert.equal((await result.stats()).sentRows, 4);
      await result.next();
      // Two of four rows are consumed, so the next batch is on its way.
      const deadline = Date.now() + 5000;
      while ((await result.stats()).sentRows < 8 && Date.now() < deadline)
        await new Promise((resolve) => setTimeout(resolve, 10));
      assert.equal((await result.stats()).sentRows, 8);
      assert.equal((await result.toArray()).length, 18);
    } finally {
      await ds.close();
    }
  },
);
