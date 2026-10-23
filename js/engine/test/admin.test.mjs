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
import { spawn } from 'node:child_process';
import { QueryEngine } from '@comunica/query-sparql-rdfjs';
import {
  Dataset,
  Catalog,
  Repository,
  factory,
  ConflictError,
  InvalidInputError,
  QueryTimeoutError,
} from '../dist/index.js';
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
test('the timeout of graph overview covers all of its queries', async (t) => {
  const ex = 'http://ex.org/';
  const ds = t.resource(Dataset.memory());
  // the answer takes many queries, each of which is shorter than the timeout
  const rows = Array.from(
    { length: 200000 },
    (_, i) =>
      `<${ex}s${i}> a <${ex}T> ; <${ex}region> <${ex}r${i % 12}> ; <${ex}knows> <${ex}s${(i * 7) % 200000}> .`,
  ).join('\n');
  await ds.load(rows, { format: 'ttl' });
  const request = { within: { class: ex + 'T' }, by: { property: ex + 'region' } };
  const timed = async () => {
    const start = performance.now();
    const answer = await ds.graph.groups(request, { noCache: true });
    assert.equal(answer.groups.length, 12);
    return performance.now() - start;
  };
  const whole = Math.min(await timed(), await timed());
  assert.ok(whole > 40, `the answer took ${whole} ms, too fast to bound`);
  await assert.rejects(
    ds.graph.groups(request, { noCache: true, timeout: Math.floor(whole / 2) }),
    QueryTimeoutError,
  );
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
