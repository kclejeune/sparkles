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
import { mkdtemp, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { QueryEngine } from '@comunica/query-sparql-rdfjs';
import { Dataset, Catalog, Repository, factory, ConflictError } from '../dist/index.js';
const temp = () => mkdtemp(join(tmpdir(), 'sparkles-node-admin-'));
test('catalog identities, alias transaction ownership and dataset recreation', async (t) => {
  const cat = t.resource(Catalog.memory());
  const ds = t.resource(await cat.create('one', { kind: 'mem' }));
  await ds.add(
    factory.quad(factory.namedNode('urn:s'), factory.namedNode('urn:p'), factory.literal('x')),
  );
  const alias = await cat.get('one');
  assert.equal(alias.id, ds.id);
  assert.equal((await cat.getById(ds.id)).id, ds.id);
  assert.equal((await cat.info('one')).id, ds.id);
  await ds.transaction(async (t) => {
    await assert.rejects(alias.update('INSERT DATA {<urn:a> <urn:b> 1}'), ConflictError);
    await assert.rejects(alias.cloneTo('/tmp/should-not-clone'), ConflictError);
  });
  await cat.rename('one', 'two');
  assert.equal(await cat.get('one'), null);
  assert.equal((await cat.get('two')).id, ds.id);
  await cat.delete('two');
  const fresh = await cat.create('two', { kind: 'mem' });
  assert.notEqual(fresh.id, ds.id);
  assert.equal(await fresh.count(), 0n);
});
test('persistent catalog/direct dataset opens share stores and paths', async (t) => {
  const path = await temp();
  try {
    const cat = await Catalog.open(path);
    const ds = await cat.create('one');
    assert.equal(ds.path, join(path, 'databases', 'one'));
    const direct = await Dataset.open(ds.path);
    assert.equal(direct.id, ds.id);
    await direct.update('INSERT DATA {<urn:s> <urn:p> 1}');
    assert.equal(await ds.count(), 1n);
    await ds.transaction(async (t) => {
      await assert.rejects(direct.update('INSERT DATA {<urn:a> <urn:b> 1}'), ConflictError);
    });
    await direct.close();
    await cat.close();
    const infos = await Catalog.inspect(path);
    assert.equal(infos[0].id, ds.id);
    const reopened = t.resource(await Catalog.open(path));
    assert.equal(await (await reopened.get('one')).count(), 1n);
  } finally {
    await t.cleanup();
    await rm(path, { recursive: true, force: true });
  }
});
test('history, snapshots, settings, branches, guards and validation handles', async (t) => {
  const path = await temp();
  try {
    const ds = t.resource(await Dataset.open(path));
    await ds.prefixes.set('ex', 'urn:');
    assert.equal((await ds.prefixes.list()).ex, 'urn:');
    await ds.update('INSERT DATA {<urn:s> <urn:p> 1}');
    const s = await ds.snapshots.create('one');
    assert.equal(s.snapshot.seq, 1n);
    assert.equal((await ds.history.commit()).seq, 1n);
    await ds.update('INSERT DATA {<urn:t> <urn:p> 2}');
    assert.equal((await ds.select('SELECT * {?s ?p ?o}', { at: 'snapshot:one' })).size, 1);
    const changes = await ds.history.changes(1n);
    assert.equal(changes.commits.length, 1);
    assert.equal(changes.commits[0].changes[0].quad.subject.value, 'urn:t');
    await ds.settings.describe.set({ mode: 'cbd' });
    assert.equal((await ds.settings.describe.get()).mode, 'cbd');
    await ds.branches.create('draft');
    const draft = t.resource(await ds.branch('draft'));
    await draft.update('INSERT DATA {<urn:b> <urn:p> 3}');
    assert.equal(await ds.count(), 2n);
    assert.equal(await draft.count(), 3n);
    assert((await ds.branches.merge('draft')).merged);
    await ds.branches.rename('draft', 'review');
    assert.equal((await ds.branches.get('review')).name, 'review');
    assert.equal(await ds.count(), 3n);
    const report = await ds.validation.shacl(
      '@prefix sh:<http://www.w3.org/ns/shacl#>. <urn:Shape> a sh:NodeShape; sh:targetNode <urn:s>; sh:property [sh:path <urn:p>;sh:minCount 1].',
    );
    assert(report.conforms);
    const guard = await ds.validation.guard.setShacl({
      mode: 'reject',
      shapes: {
        inline:
          '@prefix sh:<http://www.w3.org/ns/shacl#>. <urn:Shape> a sh:NodeShape; sh:targetNode <urn:s>; sh:property [sh:path <urn:p>;sh:minCount 1].',
      },
      reportLimit: 10,
    });
    assert(guard.installed);
    await assert.rejects(
      ds.update('DELETE WHERE {<urn:s> ?p ?o}'),
      (e) => e.details.validation && e.code === 'ERR_SPARKLES_WRITE_REJECTED',
    );
    await ds.validation.guard.reset();
  } finally {
    await t.cleanup();
    await rm(path, { recursive: true, force: true });
  }
});
test('RDF/JS Source supplies Comunica matching and native counts', async (t) => {
  const ds = t.resource(Dataset.memory());
  await ds.update('INSERT DATA {<urn:s> <urn:p> <urn:o>. <urn:o> <urn:q> 7.}');
  assert.equal(await ds.countQuads(null, factory.namedNode('urn:p')), 1n);
  assert.equal(await ds.source().countQuads(), 2);
  const engine = new QueryEngine();
  const bindings = await engine.queryBindings('SELECT ?s ?n WHERE {?s <urn:p> ?o. ?o <urn:q> ?n}', {
    sources: [ds.source()],
  });
  const rows = await bindings.toArray();
  assert.equal(rows.length, 1);
  assert.equal(rows[0].get('n').value, '7');
});
test('repository backup handles round trip independent restored identity', async (t) => {
  const path = await temp();
  try {
    const ds = t.resource(Dataset.memory());
    await ds.update('INSERT DATA {<urn:s> <urn:p> "x"}');
    const repo = t.resource(
      await Repository.open({ name: 'test', type: 'fs', path: join(path, 'repo') }),
    );
    const b = await ds.backups(repo).create('copy');
    assert.equal(b.dataset.id, ds.id);
    assert.equal((await ds.backups(repo).list()).length, 1);
    const restored = await repo.restoreToDir('copy', join(path, 'restored'));
    const copy = t.resource(await Dataset.open(join(path, 'restored')));
    assert.equal(await copy.count(), 1n);
    assert.equal(copy.id, restored.datasetId);
    assert(await ds.backups(repo).delete('copy'));
  } finally {
    await t.cleanup();
    await rm(path, { recursive: true, force: true });
  }
});
test(
  'change feed waits for commit notifications, return aborts pending next and dataset close ends waits',
  { timeout: 5000 },
  async (t) => {
    const ds = Dataset.memory();
    const feed = ds.changes();
    const first = feed.next();
    await ds.update('INSERT DATA {<urn:s> <urn:p> 1}');
    assert.equal((await first).value.commit.seq, 1n);
    const next = feed.next();
    await feed.return();
    assert((await next).done);
    const pending = ds.changes({ after: 1n });
    const waiting = pending.next();
    await ds.close();
    assert((await waiting).done);
  },
);
test('write dry runs report intended changes and plans preserve dataset state', async (t) => {
  const ds = t.resource(Dataset.memory());
  const p = await ds.update('INSERT DATA {<urn:s> <urn:p> 1}', { dryRun: { changes: 10 } });
  assert.equal(p.inserted, 1n);
  assert.equal(p.preview.outcome, 'commit');
  assert.equal(p.preview.changes.length, 1);
  assert.equal(await ds.count(), 0n);
  const explained = await ds.explain('SELECT * {?s ?p ?o}');
  assert.equal(typeof explained.text, 'string');
  const rows = await ds.select('SELECT * {?s ?p ?o}');
  assert(rows.plan);
  await rows.close();
});
test('RDF/JS Store supports Comunica updates and removals', async (t) => {
  const ds = t.resource(Dataset.memory());
  const engine = new QueryEngine();
  const store = ds.store();
  await engine.queryVoid('INSERT DATA {<urn:s> <urn:p> 1}', {
    sources: [store],
    destination: store,
  });
  assert.equal(await ds.count(), 1n);
  await engine.queryVoid('DELETE WHERE {?s ?p ?o}', { sources: [store], destination: store });
  assert.equal(await ds.count(), 0n);
});
test('stored queries and paged schema/history admin surfaces', async (t) => {
  const ds = t.resource(Dataset.memory());
  await ds.update('INSERT DATA {<urn:s> a <urn:Class>; <urn:p> 1}');
  await ds.queries.put('all', { query: 'SELECT * {?s ?p ?o}' });
  const rows = await ds.queries.run('all');
  assert.equal((await rows.toArray()).length, 2);
  assert.equal((await ds.schema.classes()).items[0].iri, 'urn:Class');
  assert((await ds.schema.predicates()).items.length > 0);
  const status = await ds.history.status();
  for (const pair of status.reconstructable) for (const n of pair) assert.equal(typeof n, 'bigint');
  assert.equal((await ds.history.query()).changes.length, 2);
  assert((await ds.branches.commitGraph()).commits.length > 0);
  const clone = t.resource(await ds.cloneToMemory());
  assert.notEqual(clone.id, ds.id);
  assert.equal(await clone.count(), 2n);
});
test('replacement is atomic and callback snapshot capture is rejected', async (t) => {
  const ds = t.resource(Dataset.memory());
  await ds.update('INSERT DATA {<urn:old> <urn:p> 1}');
  const q = factory.quad(
    factory.namedNode('urn:new'),
    factory.namedNode('urn:p'),
    factory.literal('x'),
  );
  const changed = await ds.replace([q]);
  assert.equal(changed.deleted, 1n);
  assert.equal(changed.inserted, 1n);
  assert.equal(await ds.count(), 1n);
  await ds.transaction(async () => {
    await assert.rejects(ds.cloneToMemory(), ConflictError);
  });
  const bad = {
    ...q,
    subject: factory.namedNode('urn:bad'),
    predicate: { termType: 'Literal', value: 'bad' },
  };
  await assert.rejects(ds.replace([q, bad]));
  assert(await ds.has(q));
  assert.equal(await ds.count(), 1n);
});
test('persistent catalog rename releases temporary guard leases and reopens data', async (t) => {
  const path = await temp();
  try {
    const cat = t.resource(await Catalog.open(path));
    const ds = await cat.create('before');
    await ds.update('INSERT DATA {<urn:s> <urn:p> 1}');
    const id = ds.id;
    await ds.close();
    const renamed = await cat.rename('before', 'after');
    assert.equal(renamed.id, id);
    assert.equal(renamed.path, join(path, 'databases', 'after'));
    assert.equal(await renamed.count(), 1n);
    await cat.close();
    const reopened = t.resource(await Catalog.open(path));
    assert.equal(await reopened.get('before'), null);
    const after = await reopened.get('after');
    assert.equal(after.id, id);
    assert.equal(await after.count(), 1n);
  } finally {
    await t.cleanup();
    await rm(path, { recursive: true, force: true });
  }
});
test('application JSON values and engine uint64 config round trip independently', async (t) => {
  const ds = t.resource(Dataset.memory());
  await ds.update(
    'INSERT DATA {<http://example/alice> a <http://example/Person>; <http://example/name> "Alice"; <http://example/age> 42.}',
  );
  await ds.graphql.put({
    sdl: 'extend schema @rdf(vocab: "http://example/") type Person { name: String age: Int }',
  });
  const response = await ds.graphql.execute('{ allPerson { nodes { age bytes: age } } }');
  assert.equal(response.data.allPerson.nodes[0].age, 42);
  assert.equal(response.data.allPerson.nodes[0].bytes, 42);
  await ds.queries.put('parameter', {
    query: 'SELECT ?n WHERE {}',
    parameters: { n: { type: 'integer', default: 42 } },
  });
  const stored = await ds.queries.get('parameter');
  assert.equal(stored.definition.parameters.n.default, 42);
  const result = await ds.queries.run('parameter');
  assert.equal((await result.toArray())[0].get('n').value, '42');
  const policy = await ds.settings.compaction.get();
  policy.minDeltaQuads = 100n;
  await ds.settings.compaction.set(policy);
  const read = await ds.settings.compaction.get();
  assert.equal(read.minDeltaQuads, 100n);
  await ds.settings.compaction.set(read);
});
test(
  'nested ownership and catalog close interrupt pending clone in a bounded child process',
  { timeout: 5000 },
  async () => {
    const { execFile } = await import('node:child_process');
    const { fileURLToPath } = await import('node:url');
    await new Promise((resolve, reject) =>
      execFile(
        process.execPath,
        [fileURLToPath(new URL('./nested-transactions.mjs', import.meta.url))],
        { timeout: 3000, killSignal: 'SIGKILL' },
        (error, stdout, stderr) =>
          error ? reject(new Error(`${error.message}\n${stderr}`)) : resolve(),
      ),
    );
  },
);
