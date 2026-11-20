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

test('closing SELECT and graph prefetch reports a clean stop', { timeout: 30000 }, async () => {
  const ds = Dataset.memory();
  try {
    for (const [kind, query] of [
      ['select', 'SELECT ?x { VALUES ?x { 1 2 3 } }'],
      ['construct', 'CONSTRUCT { <urn:s> <urn:p> ?x } WHERE { VALUES ?x { 1 2 3 } }'],
    ]) {
      // A one-row batch starts prefetch as soon as next() returns. Repeated closes
      // exercise both orders of the prefetch and native close tasks.
      for (let i = 0; i < 64; i++) {
        const result = await ds[kind](query, { execution: 'streaming', batchSize: 1 });
        assert.equal((await result.next()).done, false);
        await result.return();
        const { stats } = await result.stats();
        assert.equal(stats.status, 'stopped', `${kind} close ${i}`);
        assert.equal(stats.error, null);
      }
    }
  } finally {
    await ds.close();
  }
});

test('closing a failed prefetch preserves the query error', { timeout: 30000 }, async () => {
  const ds = Dataset.memory();
  try {
    for (const [kind, query] of [
      ['select', 'SELECT ?x { VALUES ?x { 1 2 3 } }'],
      ['construct', 'CONSTRUCT { <urn:s> <urn:p> ?x } WHERE { VALUES ?x { 1 2 3 } }'],
    ]) {
      const result = await ds[kind](query, {
        execution: 'streaming',
        batchSize: 1,
        maxRowsProduced: kind === 'construct' ? 4 : 2,
      });
      assert.equal((await result.next()).done, false);
      await assert.rejects(result.next(), (error) => error.name === 'BudgetExceededError');
      await result.close();
      const { stats } = await result.stats();
      assert.equal(stats.status, 'failed');
      assert.ok(stats.error);
    }
  } finally {
    await ds.close();
  }
});

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

test('queries run automatically by default and eager opts out', { timeout: 300000 }, async () => {
  const ds = Dataset.memory();
  try {
    // a small query keeps its collected result, outside and inside a transaction
    const small = await ds.select('SELECT ?x { VALUES ?x { 1 2 3 } }');
    assert.equal(small.size, 3);
    await ds.transaction(async (tx) => {
      const rows = await tx.select('SELECT ?x { VALUES ?x { 1 2 } }');
      assert.equal((await rows.toArray()).length, 2);
    });
    // a scan of a million rows from immutable blocks streams by default
    let data = '';
    for (let i = 0; i < 1_000_000; i++) data += `<urn:s${i}> <urn:p> ${i} .\n`;
    await ds.load(data, { format: 'ttl' });
    // pending changes rule streaming out, so the scan reads compacted blocks
    await ds.compact();
    const q = 'SELECT ?s ?o WHERE { ?s <urn:p> ?o }';
    const streamed = await ds.select(q);
    assert.equal(streamed.size, undefined);
    assert.equal((await streamed.next()).done, false);
    await streamed.close();
    const eager = await ds.select(q, { execution: 'eager' });
    assert.equal(eager.size, 1_000_000);
    await eager.close();
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

test('graph overview waits for a query permit', { timeout: 30000 }, async () => {
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
    await assert.rejects(
      ds.graph.groups({ within: { class: 'urn:C' } }, { noWait: true }),
      ConflictError,
    );
    await slow;
    assert.equal((await ds.graph.groups({ within: { class: 'urn:C' } })).within.size, 0);
  } finally {
    configure({ maxConcurrentQueries: availableParallelism() });
    await ds.close();
  }
});

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
    // The engine exposes no count of running queries, so probe at the original limit of 2. A
    // probe that is admitted proves a slow query has not taken its permit yet, and a
    // ConflictError proves that both have.
    const deadline = Date.now() + 10000;
    for (;;) {
      try {
        const probe = await ds.select('SELECT * { ?s ?p ?o } LIMIT 1', { noWait: true });
        await probe.toArray();
      } catch (e) {
        if (e instanceof ConflictError) break;
        throw e;
      }
      assert.ok(Date.now() < deadline, 'the slow queries did not take both permits within 10 s');
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
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

test('a slow consumer does not run out a streaming timeout', { timeout: 30000 }, async () => {
  const ds = Dataset.memory();
  try {
    const data = Array.from({ length: 200 }, (_, i) => `<urn:s${i}> <urn:p> ${i} .`).join('\n');
    await ds.load(data, { format: 'ttl' });
    const result = await ds.select('SELECT ?s ?o WHERE { ?s <urn:p> ?o }', {
      execution: 'streaming',
      timeout: 200,
      batchSize: 10,
    });
    assert.ok(await result.next());
    // The query computes batches for far less than its timeout, but the caller takes
    // longer than the timeout to ask for the rest.
    await new Promise((resolve) => setTimeout(resolve, 400));
    assert.equal((await result.toArray()).length, 199);
  } finally {
    await ds.close();
  }
});

test(
  'a streaming timeout leaves out the time before the first read',
  { timeout: 30000 },
  async () => {
    const ds = Dataset.memory();
    try {
      const data = Array.from({ length: 200 }, (_, i) => `<urn:s${i}> <urn:p> ${i} .`).join('\n');
      await ds.load(data, { format: 'ttl' });
      const result = await ds.select('SELECT ?s ?o WHERE { ?s <urn:p> ?o }', {
        execution: 'streaming',
        timeout: 200,
        batchSize: 10,
      });
      // The caller opens the result and does other work for longer than the timeout
      // before it reads the first row.
      await new Promise((resolve) => setTimeout(resolve, 400));
      assert.equal((await result.toArray()).length, 200);
    } finally {
      await ds.close();
    }
  },
);

test(
  'a streamed term that the memory budget refuses fails the batch',
  { timeout: 30000 },
  async () => {
    const ds = Dataset.memory();
    try {
      const long = 'x'.repeat(2 << 20);
      await ds.load(`<urn:s> <urn:p> "${long}" , "short" . <urn:t> <urn:p> "short" .`, {
        format: 'ttl',
      });
      const q = 'SELECT ?o WHERE { ?s <urn:p> ?o }';
      const read = async (options) => {
        const values = [];
        for await (const row of await ds.select(q, { execution: 'streaming', ...options }))
          values.push(row.get('o').value.length);
        return values.sort((a, b) => a - b);
      };
      // a term that repeats in the batch is decoded once and read by every row
      assert.deepEqual(await read({}), [5, 5, 2 << 20]);
      // the decoded literal does not fit in one mebibyte, so the stream fails
      await assert.rejects(read({ maxMemoryBytes: 1 << 20 }), /memory/i);
    } finally {
      await ds.close();
    }
  },
);

test(
  'a term decoded in an earlier batch of a streamed query asks the memory budget again',
  { timeout: 30000 },
  async () => {
    // The three arms give three batches of the query inside one batch sent to
    // JavaScript. The first arm's batch decodes its literal. The filter of the second
    // arm holds several times the literal while its batch is read. When that arm reads
    // the first arm's literal, the literal is a key that was decoded before, and
    // decoding it for the second batch has to fit beside what the filter holds, as it
    // does when the arms read three different literals and every cell is decoded. The
    // lowest budget that reads the rows is therefore the same in both cases. A key that
    // asked the budget only in its first batch would need one literal less.
    const size = 64 << 10;
    const q =
      'SELECT ?o WHERE { { <urn:a> <urn:p> ?o } UNION ' +
      '{ <urn:b> <urn:p> ?o FILTER(STRLEN(?o) + RAND() > 0) } UNION { <urn:c> <urn:p> ?o } }';
    const lowestBudget = async (letters) => {
      const ds = Dataset.memory();
      try {
        const [a, b, c] = [...letters].map((letter) => letter.repeat(size));
        await ds.load(
          `<urn:a> <urn:p> "${a}" . <urn:b> <urn:p> "${b}" . <urn:c> <urn:p> "${c}" .`,
          { format: 'ttl' },
        );
        const read = async (maxMemoryBytes) => {
          const lengths = [];
          const rows = await ds.select(q, { execution: 'streaming', maxMemoryBytes });
          for await (const row of rows) lengths.push(row.get('o').value.length);
          return lengths;
        };
        const fits = async (maxMemoryBytes) => {
          try {
            await read(maxMemoryBytes);
            return true;
          } catch (error) {
            assert.match(error.message, /memory budget/);
            return false;
          }
        };
        // a budget of one literal is refused, and one with room reads every row
        let refused = size;
        let enough = 8 << 20;
        assert.equal(await fits(refused), false);
        assert.deepEqual(await read(enough), [size, size, size]);
        while (enough - refused > 1024) {
          const middle = (refused + enough) >> 1;
          if (await fits(middle)) enough = middle;
          else refused = middle;
        }
        // the refusal just under the lowest budget does not depend on the run
        for (let run = 0; run < 20; run++)
          await assert.rejects(read(refused), /memory budget/, `run ${run}`);
        return enough;
      } finally {
        await ds.close();
      }
    };
    const repeated = await lowestBudget('xxx');
    const distinct = await lowestBudget('xyz');
    assert.ok(
      Math.abs(repeated - distinct) <= size / 4,
      `a repeated literal needs ${repeated} bytes and three literals need ${distinct}`,
    );
  },
);

test(
  'a term that repeats across the batches of a streamed query is read by every row',
  { timeout: 30000 },
  async () => {
    const ds = Dataset.memory();
    try {
      let data = '';
      for (let i = 0; i < 500; i++) data += `<urn:s${i}> <urn:p> "shared" ; <urn:q> ${i} . `;
      await ds.load(data, { format: 'ttl' });
      const q = 'SELECT ?s ?o ?n WHERE { ?s <urn:p> ?o ; <urn:q> ?n }';
      // small query batches inside one wire batch, and one wire batch per query batch
      for (const options of [{ batchSize: 4096 }, { batchSize: 7 }]) {
        const rows = [];
        for await (const row of await ds.select(q, { execution: 'streaming', ...options }))
          rows.push([row.get('s').value, row.get('o').value, row.get('n').value]);
        assert.equal(rows.length, 500);
        assert.ok(rows.every(([s, o, n]) => o === 'shared' && s === `urn:s${n}`));
      }
    } finally {
      await ds.close();
    }
  },
);
