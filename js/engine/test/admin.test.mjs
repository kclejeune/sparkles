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
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawn } from 'node:child_process';
import { QueryEngine } from '@comunica/query-sparql-rdfjs';
import {
  Dataset,
  Catalog,
  Repository,
  factory,
  BudgetExceededError,
  ConflictError,
  InvalidInputError,
  NotFoundError,
  QueryTimeoutError,
  owl,
} from '../dist/index.js';
import { native } from '../dist/native.js';
const temp = () => mkdtemp(join(tmpdir(), 'sparkles-node-admin-'));
for (const mode of ['compute-cancel', 'compute-timeout', 'slot-cancel', 'slot-timeout']) {
  test(
    `algorithm stats controls reach native ${mode}`,
    {
      skip: process.env.SPARKLES_TEST_ALGO_STATS_ENABLE !== '1',
      timeout: 15000,
    },
    async () => {
      const dir = await temp();
      const child = spawn(
        process.execPath,
        [new URL('./fixtures/algo-stats-child.mjs', import.meta.url).pathname, mode, dir],
        {
          env: { ...process.env, SPARKLES_TEST_ALGO_STATS_DIR: dir },
          stdio: ['ignore', 'pipe', 'pipe'],
        },
      );
      let output = '';
      child.stdout.on('data', (chunk) => {
        output += chunk;
      });
      child.stderr.on('data', (chunk) => {
        output += chunk;
      });
      const timer = setTimeout(() => child.kill('SIGKILL'), 12000);
      try {
        const code = await new Promise((resolve, reject) => {
          child.once('error', reject);
          child.once('close', resolve);
        });
        assert.equal(code, 0, output);
        console.log(output.trim());
      } finally {
        clearTimeout(timer);
        if (child.exitCode === null && child.signalCode === null) {
          child.kill('SIGKILL');
          await new Promise((resolve) => child.once('close', resolve));
        }
        await rm(dir, { recursive: true, force: true });
      }
    },
  );
}
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
test('relinking moves a linked branch onto main and keeps its state', async (t) => {
  const path = await temp();
  try {
    const ds = t.resource(await Dataset.open(path));
    await ds.update('INSERT DATA {<urn:a> <urn:p> 1}');
    const info = await ds.branches.create('dev');
    const dev = t.resource(await ds.branch('dev'));
    await dev.update('INSERT DATA {<urn:b> <urn:p> 2}');
    await ds.update('INSERT DATA {<urn:c> <urn:p> 3}');
    await ds.compact();
    const head = (await ds.branches.get('dev')).head;
    const cancelled = new AbortController();
    cancelled.abort();
    await assert.rejects(ds.branches.relink('dev', { signal: cancelled.signal }));
    const report = await ds.branches.relink('dev');
    assert.equal(report.mode, 'relink');
    assert.equal(BigInt(report.quads), 2n);
    const after = await ds.branches.get('dev');
    assert.equal(after.id, info.id);
    assert.deepEqual(after.head, head);
    assert.equal(await dev.count(), 2n);
    await assert.rejects(ds.branches.relink('main'));
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
test('graph algorithms and named projections', async (t) => {
  const ds = t.resource(Dataset.memory());
  await ds.update(
    'INSERT DATA {<urn:a> <urn:p> <urn:b>. <urn:b> <urn:p> <urn:c>. <urn:c> <urn:p> <urn:a>. <urn:d> <urn:p> <urn:e>}',
  );
  const saved = await ds.algorithms.projections.put('p', {
    predicates: ['urn:p'],
    direction: 'undirected',
  });
  assert.equal(saved.created, true);
  assert.equal(saved.stored.revision, 1);
  assert.equal((await ds.algorithms.projections.list()).length, 1);
  const request = { algorithm: 'wcc', projection: 'p' };
  const rows = await (await ds.algorithms.run(request)).toArray();
  assert.equal(rows.length, 5);
  assert.equal(new Set(rows.map((r) => r.get('component').value)).size, 2);
  const stats = await ds.algorithms.stats({ algorithm: 'pageRank', projection: 'p' });
  assert.equal(stats.nodes, 5);
  await assert.rejects(ds.algorithms.run({ algorithm: 'nope' }));
  await assert.rejects(
    ds.algorithms.projections.put('p', { direction: 'forward' }, { ifRevision: 7 }),
  );
  const report = await ds.algorithms.runWrite({
    algorithm: 'pageRank',
    projection: 'p',
    write: { graph: 'urn:out', predicates: { score: 'urn:rank' }, batchSize: 2 },
  });
  assert.equal(report.status, 'complete');
  assert.equal(report.triples, 5);
  assert.equal(await ds.algorithms.projections.delete('p'), true);
  assert.equal((await ds.algorithms.projections.list()).length, 0);
});
test('graph overview groups a class and opens a group', async (t) => {
  const ex = 'http://ex.org/';
  const ds = t.resource(Dataset.memory());
  await ds.update(`PREFIX ex: <${ex}>
    INSERT DATA {
      ex:a a ex:Person ; ex:livesIn ex:p1 ; ex:knows ex:b .
      ex:b a ex:Person ; ex:livesIn ex:p1 ; ex:knows ex:c .
      ex:c a ex:Person ; ex:livesIn ex:p2 .
      ex:d a ex:Person ; ex:livesIn ex:p2 .
      ex:p1 a ex:Place .
      ex:p2 a ex:Place
    }`);
  const edges = (answer, source) =>
    answer.edges
      .filter((e) => e.source === source)
      .map((e) => [e.predicate.slice(ex.length), e.target, e.count])
      .sort((a, b) => (a[0] + a[1] < b[0] + b[1] ? -1 : 1));

  const answer = await ds.graph.groups({ within: { class: ex + 'Person' }, limit: 3 });
  assert.equal(answer.commit, 1n);
  assert.deepEqual(answer.within, { class: ex + 'Person', size: 4 });
  assert.deepEqual(answer.grouping, { kind: 'values', predicate: ex + 'livesIn' });
  assert.equal(answer.communities, false);
  assert.equal(answer.truncated, false);
  assert.equal(answer.members, undefined);
  assert.deepEqual(
    answer.groups.map((g) => [g.label, g.size, g.top.map((m) => m.iri)]),
    [
      [ex + 'p1', 2, [ex + 'a', ex + 'b']],
      [ex + 'p2', 2, [ex + 'c', ex + 'd']],
    ],
  );
  const [first, second] = answer.groups.map((g) => 'g:' + g.id);
  assert.deepEqual(edges(answer, first), [
    ['knows', first, 1],
    ['knows', second, 1],
    ['livesIn', 'c:' + ex + 'Place', 2],
  ]);
  assert.deepEqual(edges(answer, second), [['livesIn', 'c:' + ex + 'Place', 2]]);
  assert.equal(answer.edges.length, 4);

  const members = await ds.graph.groups({ within: { group: answer.groups[0].id }, limit: 3 });
  assert.equal(members.within.group, answer.groups[0].id);
  assert.equal(members.within.size, 2);
  assert.deepEqual(members.grouping, { kind: 'members', predicate: null });
  assert.deepEqual(members.groups, []);
  assert.deepEqual(members.members, [
    { iri: ex + 'a', class: ex + 'Person' },
    { iri: ex + 'b', class: ex + 'Person' },
  ]);
  assert.deepEqual(edges(members, 'm:' + ex + 'a'), [
    ['knows', 'm:' + ex + 'b', 1],
    ['livesIn', 'c:' + ex + 'Place', 1],
  ]);

  const places = await ds.graph.groups({ within: { class: ex + 'Place' } });
  assert.deepEqual(
    places.members.map((m) => m.iri),
    [ex + 'p1', ex + 'p2'],
  );
  const byProperty = await ds.graph.groups({
    within: { class: ex + 'Person' },
    by: { property: ex + 'livesIn', buckets: 4 },
  });
  assert.deepEqual(byProperty, answer);

  // the point in history and the view
  await ds.snapshots.create('before');
  await ds.update(`INSERT DATA {<${ex}p3> a <${ex}Place>}`);
  assert.equal((await ds.graph.groups({ within: { class: ex + 'Place' } })).within.size, 3);
  const past = { within: { class: ex + 'Place' }, at: 'snapshot:before' };
  assert.equal((await ds.graph.groups(past)).within.size, 2);
  assert.deepEqual(await ds.graph.groups({ ...past, at: 1 }), await ds.graph.groups(past));
  const empty = await ds.graph.groups(
    { within: { class: ex + 'Place' } },
    { defaultGraph: [ex + 'none'] },
  );
  assert.equal(empty.within.size, 0);

  // errors and cancellation
  await assert.rejects(ds.graph.groups({ within: { class: 'not an iri' } }), InvalidInputError);
  await assert.rejects(ds.graph.groups({ within: { class: ex + 'Person' }, limit: 0 }));
  await assert.rejects(ds.graph.groups({ within: { class: ex + 'Person' }, by: 'nope' }));
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    ds.graph.groups({ within: { class: ex + 'Person' } }, { signal: controller.signal }),
  );
});
test('graph overview groups the dataset by its GraphRAG communities', async (t) => {
  const ex = 'http://ex.org/';
  const ds = t.resource(Dataset.memory());
  const none = await ds.graph.groups({ by: { reports: {} } });
  assert.equal(none.available, false);
  assert.equal(none.unavailable, 'none');
  await ds.update(`PREFIX ex: <${ex}>
    PREFIX gr: <urn:x-sparkles:graphrag#>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
    INSERT DATA {
      ex:a ex:knows ex:b . ex:b ex:knows ex:c .
      GRAPH <urn:x-sparkles:graphrag:reports> {
        <urn:c:A> a gr:Community ; gr:level 0 ; gr:memberCount 3 ;
          gr:member ex:a, ex:b, ex:c ; gr:report <urn:r:A> .
        <urn:r:A> rdfs:label "Team A" ; gr:status "reviewed" .
        <urn:c:A1> a gr:Community ; gr:level 1 ; skos:broader <urn:c:A> ; gr:memberCount 2 ;
          gr:member ex:a, ex:b .
      }
    }`);
  const top = await ds.graph.groups({ by: { reports: {} } });
  assert.equal(top.available, true);
  assert.deepEqual(top.grouping, { kind: 'reports', predicate: null, level: 0 });
  assert.equal(top.groups.length, 1);
  const [team] = top.groups;
  assert.deepEqual([team.label, team.size, team.children], ['Team A', 3, 1]);
  assert.deepEqual(team.report, { iri: 'urn:r:A', title: 'Team A', status: 'reviewed' });
  const below = await ds.graph.groups({ within: { group: team.id } });
  assert.deepEqual(
    below.groups.map((g) => [g.size, g.folded ?? false]),
    [
      [2, false],
      [1, true],
    ],
  );
  // a → b inside A1, b → c from A1 to the members that no child holds
  assert.deepEqual(
    below.edges.map((e) => e.count),
    [1, 1],
  );
  await assert.rejects(ds.graph.groups({ by: { reports: { level: 99 } } }), InvalidInputError);
});
test('graph overview enforces its operation timeout', async (t) => {
  const ex = 'http://ex.org/';
  const ds = t.resource(Dataset.memory());
  // Native run_admin tests cover an expired operation deadline with a generous
  // per-query timeout. This real-binding check does not predict query speed from
  // earlier calls, whose timing varies with warmup and host scheduling.
  const rows = Array.from(
    { length: 24 },
    (_, i) =>
      `<${ex}s${i}> a <${ex}T> ; <${ex}region> <${ex}r${i % 12}> ; <${ex}knows> <${ex}s${(i * 7) % 24}> .`,
  ).join('\n');
  await ds.load(rows, { format: 'ttl' });
  const request = { within: { class: ex + 'T' }, by: { property: ex + 'region' } };
  const answer = await ds.graph.groups(request, { noCache: true });
  assert.equal(answer.within.size, 24);
  assert.equal(answer.groups.length, 12);
  assert(answer.groups.every((group) => group.size === 2));
  assert.equal(
    answer.edges.reduce((sum, edge) => sum + edge.count, 0),
    24,
  );
  await assert.rejects(ds.graph.groups(request, { noCache: true, timeout: 0 }), QueryTimeoutError);
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

test('demand and why-not explanations preserve the selected mode and limits', async (t) => {
  const ds = t.resource(await Dataset.memory());
  await ds.update(
    'INSERT DATA { <http://ex.org/tom> a <http://ex.org/Cat> . <http://ex.org/Cat> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex.org/Animal> . }',
  );
  const why = await ds.reasoning.explain('<http://ex.org/tom> a <http://ex.org/Animal>', {
    source: 'demand',
    profile: 'rdfs',
  });
  assert.equal(why.entailed, true);
  assert.equal(why.source, 'demand');
  const missing = await ds.reasoning.explain('<http://ex.org/tom> a <http://ex.org/Plant>', {
    mode: 'whyNot',
    profile: 'rdfs',
    maxDepth: 2,
    maxMissing: 1,
    maxHypotheses: 3,
  });
  assert.equal(missing.mode, 'whyNot');
  assert.equal(missing.entailed, false);
  assert.ok(missing.hypotheses.length > 0);
  assert.ok(missing.hypotheses.every((h) => h.missing.length === 1));
});

test('entailment diffs and minimal justifications stay read-only', async (t) => {
  const path = await temp();
  try {
    const ds = t.resource(await Dataset.open(path));
    await ds.update(
      'INSERT DATA { <urn:Manager> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:Employee> }',
    );
    const before = (await ds.history.commit()).seq;
    await ds.update('INSERT DATA { <urn:alice> a <urn:Manager> }');
    const head = (await ds.history.commit()).seq;
    const delta = await ds.reasoning.diff(String(before), 'head', { profile: 'rdfs-simple' });
    assert.equal(delta.added, 1n);
    assert.equal(delta.removed, 0n);
    assert.equal(delta.sample.added[0][2].value, 'urn:Employee');
    assert.equal((await ds.history.commit()).seq, head);
    await ds.update(
      'INSERT DATA { <urn:CatDog> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:Cat>, <urn:Dog> . <urn:Cat> <http://www.w3.org/2002/07/owl#disjointWith> <urn:Dog> . <urn:tom> a <urn:Cat>, <urn:Dog> }',
    );
    const support = await ds.reasoning.justify({ class: 'urn:CatDog' }, { profile: 'owl-rl' });
    assert.equal(support.results[0].holds, true);
    assert.equal(support.results[0].justifications[0].facts.length, 3);
    const findings = await ds.reasoning.diagnostics({
      checks: ['disjoint-classes'],
      justify: true,
    });
    assert.equal(findings.report.findings[0].justification.facts.length, 3);
  } finally {
    await t.cleanup();
    await rm(path, { recursive: true, force: true });
  }
});

test('stored mappings and mapping runs preserve revisions and atomicity', async (t) => {
  const dir = await mkdtemp(join(tmpdir(), 'sparkles-mappings-'));
  try {
    const csv = join(dir, 'people.csv');
    await writeFile(csv, 'id,name\n1,Ann\n2,Bo\n');
    const mapping = `prefixes:
  ex: http://ex.org/
mappings:
  person:
    sources:
      - [people.csv~csv]
    s: ex:person/$(id)
    graphs: ex:g
    po:
      - [ex:name, $(name)]
`;
    const ds = t.resource(Dataset.memory());
    const mappings = ds.mappings;
    const saved = await mappings.put('people', mapping, { description: 'People' });
    assert.equal(saved.created, true);
    assert.equal(saved.stored.definition.kind, 'yarrrml');
    assert.deepEqual(saved.summary.files, ['people.csv']);
    assert.equal((await mappings.list())[0].name, 'people');
    assert.equal((await mappings.get('people')).definition.text, mapping);
    assert.equal((await mappings.versions('people')).length, 1);
    assert.equal(await mappings.get('missing'), null);
    assert.equal(await mappings.get('people', 999), null);
    await assert.rejects(mappings.run('missing'));
    await mappings.put('people', mapping, { description: 'Revised', ifVersion: 1 });
    assert.equal((await mappings.versions('people')).length, 2);
    assert.equal((await mappings.get('people', 1)).definition.description, 'People');
    await assert.rejects(mappings.put('people', mapping, { ifVersion: 999 }));
    await assert.rejects(mappings.delete('people', { ifVersion: 999 }));
    const result = await mappings.run('people', { version: 1, sources: { 'people.csv': csv } });
    assert.equal(result.stats.rows, 2n);
    assert.equal(result.stats.quads, 2n);
    assert.equal(result.receipt.committed, true);
    assert.deepEqual(result.stats.warnings, []);
    assert.deepEqual(result.stats.joins, []);
    assert.equal(await ds.count(), 2n);
    assert.equal((await mappings.lastRun('people')).version, 1n);
    await ds.transaction(async () => {
      await assert.rejects(mappings.run('people', { directory: dir }), ConflictError);
      await assert.rejects(ds.runMapping(mapping, { directory: dir }), ConflictError);
    });
    const tx = await ds.beginTransaction();
    const controller = new AbortController();
    const queued = mappings.run('people', { directory: dir, signal: controller.signal });
    controller.abort(new Error('cancel mapping'));
    await assert.rejects(queued, /cancel mapping/);
    await tx.rollback();
    assert.equal(await ds.count(), 2n);
    await assert.rejects(mappings.run('people'));
    await assert.rejects(mappings.run('people', { connections: { default: 'invalid:' } }));
    assert.equal(await ds.count(), 2n);
    await writeFile(csv, 'id,name\n1,Ann\n');
    await mappings.run('people', { directory: dir, replace: true });
    assert.equal(await ds.count(), 1n);
    assert.equal((await mappings.lastRun('people')).mode, 'replace');
    const dynamic = mapping.replace('graphs: ex:g', 'graphs: ex:$(id)');
    await mappings.put('dynamic', dynamic);
    await assert.rejects(mappings.run('dynamic', { directory: dir, replace: true }));
    assert.equal(await ds.count(), 1n);
    const other = t.resource(Dataset.memory());
    assert.equal((await other.runMapping(mapping, { directory: dir })).stats.quads, 1n);
    await assert.rejects(other.runMapping('mappings: [nope', { kind: 'yarrrml' }));
    assert.equal(await other.count(), 1n);
    assert.equal(await mappings.delete('people'), true);
    assert.equal(await mappings.get('people'), null);
    const readOnly = t.resource(Dataset.memory({ readOnly: true }));
    await assert.rejects(readOnly.mappings.put('x', mapping));
    await assert.rejects(readOnly.runMapping(mapping, { directory: dir }));
    const held = await ds.beginTransaction();
    const pending = mappings.run('dynamic', { directory: dir });
    const rejected = assert.rejects(pending);
    await ds.close();
    await rejected;
    await held.rollback();
    await assert.rejects(mappings.list());
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

// Another transaction holds the binding's writer permit, so these runs time out at
// binding admission before any mapping code runs.
test(
  'mapping runs time out at writer admission while another transaction holds the writer',
  { timeout: 60000 },
  async (t) => {
    const dir = await mkdtemp(join(tmpdir(), 'sparkles-mapping-deadline-'));
    try {
      await writeFile(join(dir, 'people.csv'), 'id,name\n1,Ann\n');
      const mapping = `prefixes:
  ex: http://ex.org/
mappings:
  person:
    sources:
      - [people.csv~csv]
    s: ex:person/$(id)
    po:
      - [ex:name, $(name)]
`;
      const ds = t.resource(Dataset.memory());
      await ds.mappings.put('people', mapping);
      const held = await ds.beginTransaction();
      for (const run of [
        () => ds.mappings.run('people', { directory: dir, timeout: 100 }),
        () => ds.runMapping(mapping, { directory: dir, timeout: 100 }),
      ]) {
        // The holder never ends during the wait, so only the operation's budget refuses.
        await assert.rejects(run(), QueryTimeoutError);
      }
      await held.rollback();
      assert.equal(await ds.count(), 0n);
      assert.equal(await ds.mappings.lastRun('people'), null);
      const done = await ds.mappings.run('people', { directory: dir, timeout: 30000 });
      assert.equal(done.receipt.committed, true);
      assert.equal(
        (await ds.runMapping(mapping, { directory: dir, timeout: 30000 })).stats.quads,
        1n,
      );
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  },
);

// The package's own timer stops a call through its cancellation flag. These calls go
// to the native dataset directly with a flag that is never set, so only the deadline
// that the addon derives from the timeout and passes to the mapping can refuse them.
// The input takes far longer than the timeout to convert, and the same calls without
// a timeout succeed. Whether the deadline passes during admission or conversion, a
// correct addon refuses, so the outcome does not depend on the host's speed. The
// library tests check that the deadline stops conversion at its next checkpoint.
test(
  'the native mapping timeout reaches the mapping write options',
  { timeout: 120000 },
  async () => {
    const dir = await mkdtemp(join(tmpdir(), 'sparkles-mapping-conversion-'));
    try {
      const rows = ['id,name'];
      for (let i = 0; i < 400000; i++) rows.push(`${i},n${i}`);
      await writeFile(join(dir, 'people.csv'), rows.join('\n') + '\n');
      const text = `prefixes:
  ex: http://ex.org/
mappings:
  person:
    sources:
      - [people.csv~csv]
    s: ex:person/$(id)
    po:
      - [ex:name, $(name)]
`;
      const admin = (ds, op, args, options = {}) =>
        ds.admin(op, JSON.stringify(args), JSON.stringify(options), new native.Cancellation());
      const runs = [
        ['mappings.run', { name: 'people', directory: dir }],
        ['runMapping', { text, directory: dir }],
      ];
      const ds = native.NativeDataset.memory('{}');
      await admin(ds, 'mappings.put', { name: 'people', text });
      for (const [op, args] of runs)
        await assert.rejects(admin(ds, op, args, { timeout: 50 }), /time/i, op);
      assert.equal(JSON.parse(await admin(ds, 'info', {})).quads, '0');
      assert.equal(JSON.parse(await admin(ds, 'mappings.lastRun', { name: 'people' })), null);
      for (const [op, args] of runs) {
        const control = native.NativeDataset.memory('{}');
        await admin(control, 'mappings.put', { name: 'people', text });
        const run = JSON.parse(await admin(control, op, args));
        assert.equal(run.receipt.committed, true, op);
      }
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  },
);

// Through the package, the timer stops the call through the cancellation flag that
// the mapping write options carry into conversion.
test('the package mapping timeout refuses both mapping runs', { timeout: 120000 }, async (t) => {
  const dir = await mkdtemp(join(tmpdir(), 'sparkles-mapping-conversion-'));
  try {
    const rows = ['id,name'];
    for (let i = 0; i < 400000; i++) rows.push(`${i},n${i}`);
    await writeFile(join(dir, 'people.csv'), rows.join('\n') + '\n');
    const mapping = `prefixes:
  ex: http://ex.org/
mappings:
  person:
    sources:
      - [people.csv~csv]
    s: ex:person/$(id)
    po:
      - [ex:name, $(name)]
`;
    const ds = t.resource(Dataset.memory());
    await ds.mappings.put('people', mapping);
    await assert.rejects(
      ds.mappings.run('people', { directory: dir, timeout: 50 }),
      QueryTimeoutError,
    );
    await assert.rejects(
      ds.runMapping(mapping, { directory: dir, timeout: 50 }),
      QueryTimeoutError,
    );
    assert.equal(await ds.count(), 0n);
    assert.equal(await ds.mappings.lastRun('people'), null);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('the ontology handle reads, checks, edits and diffs axioms', async (t) => {
  // on disk, so that the diff can read the earlier commit
  const ds = t.resource(await Dataset.open(join(await temp(), 'db')));
  await ds.update(`PREFIX : <http://ex.org/>
    PREFIX owl: <http://www.w3.org/2002/07/owl#>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    INSERT DATA { :o a owl:Ontology . :A a owl:Class . :B a owl:Class . :C a owl:Class .
      :p a owl:ObjectProperty .
      :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .
      :a a :A . }`);
  const onto = ds.ontology;
  const summary = await onto.summary();
  assert.equal(summary.header.iri, 'http://ex.org/o');
  assert.equal(Number(summary.axioms.SubClassOf), 1);
  const page = await onto.axiomPage({ type: ['SubClassOf'] });
  assert.equal(Number(page.total), 1);
  const [entry] = page.axioms;
  assert.equal(entry.type, 'SubClassOf');
  assert.equal(entry.axiom.type, 'SubClassOf');
  assert.equal(entry.axiom.superClass.type, 'ObjectSomeValuesFrom');
  // the listing is an async iterable in batches
  const listed = [];
  for await (const a of onto.axioms({ assertions: true, batch: 2 })) listed.push(a.type);
  assert.ok(listed.includes('ClassAssertion'), listed.join());
  assert.equal(listed.length, Number((await onto.axiomPage({ assertions: true })).total));
  const check = await onto.check({ profiles: ['el', 'rl'] });
  assert.deepEqual(
    check.reports.map((r) => r.profile),
    ['el', 'rl'],
  );
  const before = summary.commit;
  const done = await onto.edit({
    remove: [entry.axiom],
    add: ['SubClassOf(<http://ex.org/B> <http://ex.org/C>)'],
  });
  assert.equal(Number(done.removed), 1);
  assert.equal(Number(done.added), 1);
  const diff = await onto.diff(before);
  assert.equal(Number(diff.addedCount), 1);
  assert.equal(diff.removed[0].type, 'SubClassOf');
  const gone = await onto.removeEntity('http://ex.org/C');
  assert.ok(Number(gone.removed) >= 1);
  await assert.rejects(onto.edit({}), InvalidInputError);
  await assert.rejects(onto.axiomPage({ about: 'http://ex.org/nobody' }));
  // an inferred read says whether it found materialized inferences
  const inferred = await onto.summary({ inferences: true });
  assert.equal(inferred.inferences.included, false);
  // edits and diffs refuse an inferred or historical read, and commit nothing
  const head = (await onto.summary()).commit;
  const add = { add: ['Declaration(Class(<http://ex.org/Y>))'] };
  await assert.rejects(onto.edit(add, { inferences: true }), InvalidInputError);
  await assert.rejects(onto.edit(add, { at: 'commit:1' }), InvalidInputError);
  await assert.rejects(
    onto.removeEntity('http://ex.org/A', { inferences: true }),
    InvalidInputError,
  );
  await assert.rejects(onto.diff(before, { inferences: true }), InvalidInputError);
  assert.equal((await onto.summary()).commit, head);
  // a listing at the head reads the commit of its first batch, whatever commits meanwhile
  const total = Number((await onto.axiomPage({ at: 'head' })).total);
  let seen = 0;
  for await (const _ of onto.axioms({ at: 'head', batch: 1 })) {
    if (seen++ === 0) await onto.edit({ add: ['Declaration(Class(<http://ex.org/Z>))'] });
  }
  assert.equal(seen, total);
  // the timeout bounds the whole listing, not each batch
  await assert.rejects(async () => {
    for await (const _ of onto.axioms({ batch: 1, timeout: 50 })) {
      await new Promise((r) => setTimeout(r, 60));
    }
  }, QueryTimeoutError);
});

test('the ontology handle classifies by OWL 2 EL and writes the taxonomy', async (t) => {
  const ds = t.resource(await Dataset.open(join(await temp(), 'db')));
  await ds.update(`PREFIX : <http://ex.org/>
    PREFIX owl: <http://www.w3.org/2002/07/owl#>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    INSERT DATA { :o a owl:Ontology . :A a owl:Class . :B a owl:Class . :C a owl:Class .
      :p a owl:ObjectProperty .
      :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .
      :C owl:equivalentClass [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] . }`);
  const onto = ds.ontology;
  const r = await onto.classify({ about: 'http://ex.org/A' });
  assert.equal(r.consistent, true);
  assert.equal(r.complete, true);
  assert.equal(r.reused, false);
  assert.ok(r.class.superClasses.includes('http://ex.org/C'), r.class.superClasses.join());
  assert.equal((await onto.classify()).reused, true);
  const page = await onto.classify({ nodes: true, limit: 2 });
  assert.equal(page.nodes.length, 2);
  await assert.rejects(onto.classify({ about: 'http://ex.org/nobody' }));
  const written = await onto.classify({ target: 'http://ex.org/taxonomy', message: 'classified' });
  assert.equal(written.written.committed, true);
  const tax = await onto.axiomPage({ graph: 'http://ex.org/taxonomy', type: ['SubClassOf'] });
  assert.ok(Number(tax.total) >= 1);
});

/** Item 603's cases, as the Rust tests write them: a classification the disjunctive rules take. */
function cases(n, inconsistent) {
  const some = (p, c) => `[ a owl:Restriction ; owl:onProperty ${p} ; owl:someValuesFrom ${c} ]`;
  const out = [':r a owl:ObjectProperty . :s a owl:ObjectProperty .'];
  for (let i = 0; i < n; i++) {
    const [b, d] = [(7 * i + 3) % n, (13 * i + 5) % n];
    out.push(
      `:C${i} a owl:Class ; rdfs:subClassOf [ owl:unionOf ( :B${i} :E${i} ) ] .
       :B${i} rdfs:subClassOf :K${i} , :C${b} . :E${i} rdfs:subClassOf :K${i} .
       :K${i} rdfs:subClassOf ${some(':r', `:C${d}`)} .
       :E${i} rdfs:subClassOf [ a owl:Restriction ;
         owl:onProperty [ owl:inverseOf :r ] ; owl:allValuesFrom :F${d} ] .
       ${some(':r', `:F${i}`)} rdfs:subClassOf :G${b} .`,
    );
  }
  if (inconsistent) {
    out.push(`owl:Thing rdfs:subClassOf ${some(':s', '[ owl:unionOf ( :N1 :N2 ) ]')} .
      :N1 rdfs:subClassOf owl:Nothing . :N2 rdfs:subClassOf :N3 . :N3 owl:disjointWith :N2 .`);
  }
  return `PREFIX : <http://ex.org/>
    PREFIX owl: <http://www.w3.org/2002/07/owl#>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    INSERT DATA { ${out.join('\n')} }`;
}

/**
 * Classify the cases once in full, and give the full answer, the time of the call and what
 * a partial answer is checked against.
 */
async function wholeRun(t, inconsistent) {
  const body = cases(300, inconsistent);
  // the quickest of three full runs, each on a dataset of its own, so that a loaded
  // machine's slow first run does not push every deadline past the saturation
  let full;
  let took = Infinity;
  for (let k = 0; k < 3; k++) {
    const whole = t.resource(Dataset.memory());
    await whole.update(body);
    const start = performance.now();
    full = await whole.ontology.classify({ partial: true, threads: 1, limit: 10000 });
    took = Math.min(took, performance.now() - start);
  }
  const want = inconsistent ? 'inconsistent' : 'consistent';
  assert.equal(full.complete, true);
  assert.equal(full.partial, false);
  assert.equal(full.consistency, want);
  assert.equal(full.tier, 'disjunctive');
  assert.deepEqual(full.incomplete, []);
  return { body, full, took, want };
}

/** Check an answer to a stopped classification against the full one, and return its consistency when it is partial. */
function checkAnswer(r, { full, want }) {
  assert.equal(r.classes, full.classes);
  if (!r.partial) {
    assert.equal(r.complete, true);
    assert.equal(r.consistency, want);
    return undefined;
  }
  assert.equal(r.complete, false, 'a partial answer is presented as complete');
  assert.equal(r.incompleteCount, r.incomplete.length);
  assert.ok([want, 'unknown'].includes(r.consistency), r.consistency);
  if (r.consistency === 'unknown') assert.ok(r.incomplete.length + 2 >= r.classes);
  return r.consistency;
}

/**
 * Find the saturation of a classification by the time that stops it. The times rise by a
 * few percent from a short one until one gives an answer instead of a stop before the
 * answer, which has run out in the saturation, since that is the first part of a run that
 * can stop with an answer. The step may be longer than the saturation, so the
 * milliseconds between the last time that failed and that answer are halved until they
 * are one apart. The times go on until twenty have answered, or `limit` milliseconds. A
 * ladder fixed from the time of a full run misses a saturation of some milliseconds on a
 * loaded machine. `answers` runs a classification stopped at a time and says whether it
 * answered.
 */
async function saturationLadder(limit, answers) {
  let answered = 0;
  const run = async (ms) => {
    const ok = await answers(ms);
    if (ok) answered++;
    return ok;
  };
  const next = (ms) => Math.ceil(ms + Math.max(1, 0.08 * ms));
  let lo = 0;
  let ms = 2;
  while (ms < limit && !(await run(ms))) {
    lo = ms;
    ms = next(ms);
  }
  for (let hi = ms; hi - lo > 1;) {
    const mid = Math.floor((lo + hi) / 2);
    if (await run(mid)) hi = mid;
    else lo = mid;
  }
  while (answered < 20 && ms < limit) {
    ms = next(ms);
    await run(ms);
  }
}

/**
 * Classify the cases under the deadlines of [saturationLadder]. Return the partial
 * answers' consistencies, in the order of the deadlines.
 */
async function deadlineAnswers(t, inconsistent) {
  const whole = await wholeRun(t, inconsistent);
  const seen = [];
  await saturationLadder(Math.max(2000, 20 * whole.took), async (ms) => {
    // a dataset of its own, whose read of the ontology takes as long as the full run's
    const ds = Dataset.memory();
    await ds.update(whole.body);
    let r;
    try {
      r = await ds.ontology.classify({ partial: true, threads: 1, limit: 10000, timeout: ms });
    } catch (e) {
      if (e instanceof QueryTimeoutError) return false;
      throw e;
    } finally {
      await ds.close();
    }
    const c = checkAnswer(r, whole);
    if (c) seen.push(c);
    return true;
  });
  return seen;
}

/**
 * Classify the cases on one thread under memory budgets from a small part of the full run's
 * estimate to nearly all of it, which stop the saturation at the same place every time, and
 * return the partial answers' consistencies. A memory stop answers in part as a deadline
 * does, and it does not depend on the speed of the machine.
 */
async function budgetAnswers(t, inconsistent) {
  const whole = await wholeRun(t, inconsistent);
  const estimate = whole.full.stats.estimatedBytes;
  const seen = [];
  for (let k = 1; k < 20; k++) {
    const ds = Dataset.memory();
    await ds.update(whole.body);
    let r;
    try {
      r = await ds.ontology.classify({
        partial: true,
        threads: 1,
        limit: 10000,
        maxMemoryBytes: Math.floor((estimate * k) / 20),
      });
    } catch (e) {
      // a budget under the size of the ontology and its index refuses the run
      if (e instanceof BudgetExceededError) continue;
      throw e;
    } finally {
      await ds.close();
    }
    // a budget past the one of the disjunctive rules runs the EL rules instead, whose answer
    // leaves the disjunctive axioms out, so it is neither complete nor partial
    if (!r.partial && !r.complete) {
      assert.ok(r.excludedCount > 0);
      continue;
    }
    const c = checkAnswer(r, whole);
    if (c) seen.push(c);
  }
  return seen;
}

test('the ontology handle answers a classification in part under a deadline', async (t) => {
  assert.ok((await deadlineAnswers(t, false)).length > 0, 'no deadline gave a partial answer');
  // The stop that leaves the consistency unknown comes before the saturation of owl:Thing,
  // which is the last of it here, so a stop of any kind in most of the saturation gives
  // it. A budget stops at the same place on every machine, where a deadline lands in the
  // few milliseconds of the saturation by the clock.
  const budget = await budgetAnswers(t, true);
  assert.ok(budget.includes('unknown'), 'no budget left the consistency unknown');
  assert.ok(
    (await budgetAnswers(t, false)).length > 0,
    'no budget gave a partial answer to a consistent ontology',
  );
  // the same ontology under deadlines, which check every answer and may meet the unknown
  await deadlineAnswers(t, true);
  const ds = t.resource(Dataset.memory());
  await ds.update(`PREFIX : <http://ex.org/>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    INSERT DATA { :A rdfs:subClassOf :B . }`);
  const r = await ds.ontology.classify({ partial: true, expansion: 'eager' });
  assert.deepEqual(
    [r.complete, r.partial, r.consistency, r.tier, r.incomplete],
    [true, false, 'consistent', 'el', []],
  );
  await assert.rejects(ds.ontology.classify({ partial: true, target: 'http://ex.org/taxonomy' }));
});

test('a classification answers in part when its signal aborts it', async (t) => {
  const whole = await wholeRun(t, false);
  let partial = 0;
  await saturationLadder(Math.max(2000, 20 * whole.took), async (ms) => {
    const ds = Dataset.memory();
    await ds.update(whole.body);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), ms);
    try {
      const r = await ds.ontology.classify({
        partial: true,
        threads: 1,
        limit: 10000,
        signal: controller.signal,
      });
      if (checkAnswer(r, whole)) partial++;
      return true;
    } catch (e) {
      if (!controller.signal.aborted) throw e;
      return false;
    } finally {
      clearTimeout(timer);
      await ds.close();
    }
  });
  assert.ok(partial > 0, 'no abort gave a partial answer');
});

test('the ontology handle reads Manchester syntax and answers DL queries', async (t) => {
  const ds = t.resource(await Dataset.open(join(await temp(), 'db')));
  await ds.update(`PREFIX : <http://ex.org/>
    PREFIX owl: <http://www.w3.org/2002/07/owl#>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    INSERT DATA { :o a owl:Ontology . :A a owl:Class . :B a owl:Class . :C a owl:Class .
      :p a owl:ObjectProperty . :A rdfs:subClassOf :B . :B rdfs:label "Big thing" .
      :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .
      :a a :A . }`);
  const onto = ds.ontology;
  const p = await onto.parse("p some 'Big thing'");
  assert.deepEqual(p.value, {
    type: 'ObjectSomeValuesFrom',
    property: 'http://ex.org/p',
    class: 'http://ex.org/B',
  });
  await assert.rejects(onto.parse('p some Nothing2'), (e) => {
    assert.equal(e.name, 'InvalidInputError');
    assert.match(e.message, /Nothing2/);
    assert.equal(e.details.reason, 'unknown');
    assert.deepEqual([e.details.start, e.details.end], [7, 15]);
    assert.deepEqual(e.details.candidates, []);
    return true;
  });
  const r = await onto.dlQuery('p some B', { answers: ['subclasses', 'instances'] });
  assert.equal(r.reasoning.engine, 'el');
  assert.equal(r.reasoning.complete, true);
  assert.deepEqual(r.subclasses, ['http://ex.org/A']);
  assert.deepEqual(r.instances, ['http://ex.org/a']);
  const rl = await onto.dlQuery('p some B', { reasoning: 'rl', answers: ['subclasses'] });
  assert.equal(rl.reasoning.engine, 'rl');
});

test('the ontology handle lists entities and answers entity accessors', async (t) => {
  const ds = t.resource(await Dataset.open(join(await temp(), 'db')));
  await ds.update(`PREFIX : <http://ex.org/>
    PREFIX owl: <http://www.w3.org/2002/07/owl#>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    INSERT DATA { :o a owl:Ontology . :A a owl:Class . :B a owl:Class .
      :p a owl:ObjectProperty . :p rdfs:domain :A .
      :A rdfs:subClassOf :B . :A rdfs:label "A" . :a a :A . }`);
  const onto = ds.ontology;
  const classes = await onto.entities({ kind: ['Class'] });
  assert.deepEqual(
    classes.entities.map((e) => e.iri),
    ['http://ex.org/A', 'http://ex.org/B'],
  );
  const a = await onto.entity('http://ex.org/A');
  assert.deepEqual(a.kinds, ['Class']);
  assert.deepEqual(
    a.class.superClasses.map((c) => c.expression),
    ['http://ex.org/B'],
  );
  assert.equal(a.labels[0]['@value'], 'A');
  assert.deepEqual(a.class.instances, []);
  const streamed = await onto.entity('http://ex.org/A', { assertions: true });
  assert.deepEqual(
    streamed.class.instances.map((i) => i.expression),
    ['http://ex.org/a'],
  );
  const p = await onto.entity(factory.namedNode('http://ex.org/p'), { kind: 'ObjectProperty' });
  assert.deepEqual(
    p.objectProperty.domains.map((c) => c.expression),
    ['http://ex.org/A'],
  );
  await assert.rejects(onto.entity('http://ex.org/nothing'), NotFoundError);
});

test('the ontology handle searches entities with typos', async (t) => {
  const ds = t.resource(await Dataset.open(join(await temp(), 'db')));
  await ds.update(`PREFIX : <http://ex.org/>
    PREFIX owl: <http://www.w3.org/2002/07/owl#>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
    INSERT DATA { :Glucose a owl:Class ; rdfs:label "glucose" .
      :Galactose a owl:Class ; rdfs:label "galactose" .
      :sweet a skos:Concept ; skos:prefLabel "sweet"@en ; skos:altLabel "sugary"@en . }`);
  const onto = ds.ontology;
  const prefix = await onto.search('gluc');
  assert.equal(prefix.results[0].iri, 'http://ex.org/Glucose');
  assert.equal(prefix.results[0].type, 'entity');
  assert.equal(prefix.results[0].match.kind, 'prefix');
  assert.deepEqual(prefix.results[0].match.highlight, [[0, 4]]);
  // the commit and the next offset are bigints, the other numbers plain numbers
  assert.equal(typeof prefix.commit, 'bigint');
  assert.equal(typeof prefix.offset, 'number');
  assert.equal(typeof prefix.limit, 'number');
  assert.equal(typeof prefix.results[0].match.distance, 'number');
  assert.equal(typeof prefix.results[0].match.tier, 'number');
  const typo = await onto.search('glucoes');
  assert.equal(typo.results[0].iri, 'http://ex.org/Glucose');
  assert.equal(typo.results[0].match.kind, 'fuzzy');
  const concepts = await onto.search('sugary', { kind: 'concept', lang: ['en'] });
  assert.deepEqual(
    concepts.results.map((h) => [h.iri, h.match.field]),
    [['http://ex.org/sweet', 'synonym']],
  );
  const none = await onto.search('gluc', { kind: ['concept'] });
  assert.deepEqual(none.results, []);
});

test('the ontology handle reads one hierarchy level and one entity', async (t) => {
  const ds = t.resource(await Dataset.open(join(await temp(), 'db')));
  await ds.update(`PREFIX : <http://ex.org/>
    PREFIX owl: <http://www.w3.org/2002/07/owl#>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    INSERT DATA { :Animal a owl:Class ; rdfs:label "animal" .
      :Dog a owl:Class ; rdfs:label "dog" ; rdfs:subClassOf :Animal . }`);
  const onto = ds.ontology;
  const roots = await onto.hierarchy();
  assert.equal(roots.children[0].iri, 'http://ex.org/Animal');
  assert.equal(roots.children[0].children, 1);
  assert.equal(typeof roots.commit, 'bigint');
  const up = await onto.hierarchy({ iri: 'http://ex.org/Dog', ancestors: true });
  assert.deepEqual(up.ancestors.edges, [['http://ex.org/Dog', 'http://ex.org/Animal']]);
  const term = await onto.term('http://ex.org/Dog');
  assert.equal(term.label, 'dog');
  assert.deepEqual(term.kinds, ['class']);
  assert.ok(term.axioms.some((a) => a.type === 'SubClassOf'));
  assert.equal(term.names['http://ex.org/Animal'].label, 'animal');
});

test('owl literals of numbers take XML Schema lexical forms', () => {
  const XSD = 'http://www.w3.org/2001/XMLSchema#';
  assert.deepEqual(owl.Literal(1e21), { '@value': '1' + '0'.repeat(21), '@type': XSD + 'integer' });
  assert.deepEqual(owl.Literal(Infinity), { '@value': 'INF', '@type': XSD + 'double' });
  assert.equal(owl.Literal(-Infinity)['@value'], '-INF');
  assert.equal(owl.Literal(NaN)['@value'], 'NaN');
  assert.equal(owl.Literal(1.5)['@value'], '1.5');
  assert.equal(owl.Literal(Infinity, { datatype: XSD + 'float' })['@value'], 'INF');
});

test('the owl constructors build axioms an edit takes', async (t) => {
  const ds = t.resource(await Dataset.open(join(await temp(), 'db')));
  const EX = 'http://ex.org/';
  const ax = owl.SubClassOf(
    owl.Class(EX + 'Professor'),
    owl.ObjectSomeValuesFrom(owl.ObjectProperty(EX + 'teaches'), owl.Class(EX + 'Course')),
    { annotations: [owl.Annotation('http://www.w3.org/2000/01/rdf-schema#comment', 'why')] },
  );
  assert.deepEqual(ax, {
    type: 'SubClassOf',
    subClass: EX + 'Professor',
    superClass: { type: 'ObjectSomeValuesFrom', property: EX + 'teaches', class: EX + 'Course' },
    annotations: [
      { property: 'http://www.w3.org/2000/01/rdf-schema#comment', value: { '@value': 'why' } },
    ],
  });
  const added = [
    ax,
    owl.Declaration(owl.DataProperty(EX + 'age')),
    owl.DataPropertyRange(
      EX + 'age',
      owl.DatatypeRestriction(owl.XSD + 'integer', [owl.XSD + 'minInclusive', 0]),
    ),
    owl.SubObjectPropertyOf(
      owl.ObjectPropertyChain(EX + 'p', owl.ObjectInverseOf(EX + 'q')),
      EX + 'r',
    ),
    owl.HasKey(EX + 'Professor', [], [EX + 'age']),
    owl.DataPropertyAssertion(EX + 'age', EX + 'ann', 42),
    owl.AnnotationAssertion(
      'http://www.w3.org/2000/01/rdf-schema#seeAlso',
      EX + 'ann',
      owl.IRI(EX + 'bob'),
    ),
    owl.DisjointClasses([EX + 'Professor', owl.ObjectComplementOf(EX + 'Person')]),
  ];
  const done = await ds.ontology.edit({ add: added });
  assert.ok(Number(done.added) >= added.length);
  const page = await ds.ontology.axiomPage({ limit: 1000 });
  const ofn = page.axioms.map((a) => a.ofn);
  assert.ok(
    ofn.some((s) => s.includes('ObjectPropertyChain(')),
    ofn.join('\n'),
  );
  assert.ok(
    ofn.some((s) => s.includes('xsd:minInclusive "0"^^xsd:integer')),
    ofn.join('\n'),
  );
  assert.throws(() => owl.Declaration(EX + 'A'), TypeError);
  assert.throws(() => owl.ObjectMinCardinality(-1, EX + 'p'), TypeError);
});

test('individual pages preserve graph scope, empty cursors and retained revisions', async (t) => {
  const ds = t.resource(await Dataset.open(join(await temp(), 'individual-pages')));
  const ex = 'http://ex.org/';
  const graph = ex + 'individuals';
  const prefixes = `PREFIX : <${ex}> PREFIX owl: <http://www.w3.org/2002/07/owl#>`;
  await ds.update(`${prefixes} INSERT DATA { GRAPH <${graph}> {
    :Person a owl:Class . :loose a owl:NamedIndividual .
    :a a :Person . :b a :Person . :c a :Person .
  } GRAPH :hidden { :hidden a :Person } }`);
  const scope = { graph, imports: false };
  const roots = await ds.ontology.individuals({ ...scope, limit: 1 });
  assert.deepEqual(roots.groups, [{ type: null, total: 1 }]);
  assert.equal(roots.next, '');
  assert.equal(typeof roots.commit, 'bigint');
  const at = roots.commit;
  const types = await ds.ontology.individuals({
    ...scope,
    limit: 1,
    after: roots.next,
    at,
  });
  assert.deepEqual(
    types.groups.map((g) => g.type.iri),
    [ex + 'Person'],
  );
  assert.equal(types.next, null);
  const first = await ds.ontology.individuals({
    ...scope,
    type: ex + 'Person',
    limit: 2,
    at,
  });
  assert.deepEqual(
    first.individuals.map((i) => i.iri),
    [ex + 'a', ex + 'b'],
  );
  assert.equal(first.next, ex + 'b');
  assert.equal(first.total, 3);
  await ds.update(`${prefixes} INSERT DATA { GRAPH <${graph}> { :d a :Person } }`);
  const last = await ds.ontology.individuals({
    ...scope,
    type: ex + 'Person',
    limit: 2,
    after: first.next,
    at,
  });
  assert.equal(last.commit, at);
  assert.equal(last.next, null);
  assert.deepEqual(
    last.individuals.map((i) => i.iri),
    [ex + 'c'],
  );
  const located = await ds.ontology.individuals({
    ...scope,
    iri: ex + 'c',
    at,
  });
  assert.deepEqual(
    located.individuals[0].types.map((type) => type.iri),
    [ex + 'Person'],
  );
});
