import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import {
  Catalog,
  checkIri,
  checkLangtag,
  checkData,
  parseQuery,
  parseUpdate,
  format,
  lint,
  convertGeometries,
  previewSchedule,
  InvalidInputError,
  SparqlSyntaxError,
  ConflictError,
  QueryTimeoutError,
  UnsupportedError,
} from '../dist/index.js';

test('standalone term checks distinguish errors and warnings', async () => {
  assert.deepEqual((await checkIri('urn:s')).errors, []);
  assert.ok((await checkIri('../relative')).warnings.length > 0);
  assert.ok((await checkIri('http://bad iri')).errors.length > 0);
  const tag = await checkLangtag('en-us');
  assert.equal(tag.canonical, 'en-US');
  assert.equal(tag.region, 'us');
  assert.equal(tag.error, null);
  assert.ok((await checkLangtag('bad tag')).error);
});
test('standalone SPARQL parsers use engine extensions, prefixes and base IRIs', async () => {
  const query = await parseQuery('SELECT ?o WHERE { ex:s ex:p ?o }', { prefixes: { ex: 'urn:' } });
  assert.match(query, /<urn:s>/);
  const update = await parseUpdate('INSERT DATA { <s> <p> 1 }', {
    baseIri: 'https://example.org/',
  });
  assert.match(update, /https:\/\/example.org\/s/);
  await assert.rejects(parseQuery('SELECT WHERE'), SparqlSyntaxError);
  await assert.rejects(parseUpdate('INSERT nonsense'), SparqlSyntaxError);
});
test('RDF syntax validation gives lossless positions and rejects invalid format/base', async () => {
  assert.equal(await checkData('<s> <p> 1 .', 'ttl', { baseIri: 'https://example.org/' }), null);
  const issue = await checkData('<urn:s> <urn:p> .', 'nt');
  assert.equal(typeof issue.line, 'bigint');
  assert.ok(issue.line > 0n);
  assert.ok(issue.message.length > 0);
  await assert.rejects(checkData('', 'garbage'), InvalidInputError);
  await assert.rejects(checkData('', 'ttl', { baseIri: 'bad base' }), InvalidInputError);
});
test('formatter preserves comments, maps cursors and validates option enums/ranges', async () => {
  const input = '# preserve\nSELECT * WHERE {?s ?p ?o}';
  const result = await format(input, 'sparql', {
    indentWidth: 4,
    cursor: input.indexOf('?s'),
    operatorPosition: 'trailing',
  });
  assert.match(result.text, /# preserve/);
  assert.equal(typeof result.cursor, 'number');
  assert.equal(result.text.slice(result.cursor, result.cursor + 2), '?s');
  assert.equal(
    (await format(result.text, 'sparql', { indentWidth: 4, operatorPosition: 'trailing' })).changed,
    false,
  );
  await assert.rejects(format(input, 'sparql', { lineWidth: 1 }), InvalidInputError);
  await assert.rejects(format(input, 'sparql', { quoteStyle: 'bad' }), InvalidInputError);
  await assert.rejects(format(input, 'sparql', { unknown: true }), InvalidInputError);
  await assert.rejects(format(input, 'sparql', { timeout: 0 }), QueryTimeoutError);
});
test('linter returns source diagnostics and honors rule severities', async () => {
  const input = 'PREFIX ex: <urn:> SELECT * WHERE { ?s ?p ?o }';
  const result = await lint(input, 'sparql');
  const unused = result.diagnostics.find((d) => d.rule === 'unused-prefix');
  assert.equal(unused.severity, 'warning');
  assert.ok(unused.fix.edits.length > 0);
  const off = await lint(input, 'sparql', { levels: { 'unused-prefix': 'off' } });
  assert.equal(
    off.diagnostics.some((d) => d.rule === 'unused-prefix'),
    false,
  );
  await assert.rejects(lint(input, 'sparql', { levels: { syntax: 'off' } }), InvalidInputError);
  assert.ok((await lint('SELECT WHERE', 'sparql')).diagnostics.some((d) => d.severity === 'error'));
});
test('geometry helpers preserve numeric coordinates and per-item failures', async () => {
  const converted = await convertGeometries([
    { value: 'POINT(1 2)', datatype: 'http://www.opengis.net/ont/geosparql#wktLiteral' },
    { value: 'bad', datatype: 'urn:unsupported' },
  ]);
  assert.deepEqual(converted[0].geometry, { type: 'Point', coordinates: [1, 2] });
  assert.equal(typeof converted[1].error, 'string');
});
test('schedule preview is deterministic and bounded', async () => {
  assert.deepEqual(await previewSchedule('every 6h', { count: 2, after: '2026-01-01T00:00:00Z' }), [
    '2026-01-01T06:00:00+00:00',
    '2026-01-01T12:00:00+00:00',
  ]);
  assert.deepEqual(await previewSchedule('every 6h', { count: 0 }), []);
  assert.throws(() => previewSchedule('every 6h', { count: 1001 }), InvalidInputError);
  await assert.rejects(previewSchedule('every 1s'));
  await assert.rejects(previewSchedule('every 6h', { after: 'not a date' }), InvalidInputError);
});

test('catalog reservations prevent collisions and release explicitly or on close', async () => {
  const cat = Catalog.memory();
  try {
    const claim = cat.reserve('claimed', 'clone', '123');
    await assert.rejects(cat.create('claimed', { kind: 'mem' }), ConflictError);
    assert.throws(() => cat.reserve('claimed', 'clone', 'other'), ConflictError);
    claim.close();
    claim.close();
    const ds = await cat.create('claimed', { kind: 'mem' });
    const replacement = cat.reserve('claimed', 'restore', 'restore-owner');
    assert.equal((await cat.info('claimed')).reservedBy, 'restore-owner');
    await assert.rejects(cat.delete('claimed'), ConflictError);
    replacement.close();
    assert.equal((await cat.info('claimed')).reservedBy, null);
    const last = cat.reserve('last', 'clone', 'last-owner');
    await ds.close();
    await cat.close();
    last.close();
  } finally {
    await cat.close();
  }
});
test('catalog backupFiles collects sorted regular legacy files only', async () => {
  const path = await mkdtemp(join(tmpdir(), 'sparkles-backup-files-'));
  const cat = await Catalog.open(path);
  try {
    await mkdir(join(path, 'backups', 'nested'), { recursive: true });
    await writeFile(join(path, 'backups', 'b'), 'b');
    await writeFile(join(path, 'backups', 'a'), 'a');
    await writeFile(join(path, 'backups', '.hidden'), 'hidden');
    assert.deepEqual(
      await cat.backupFiles(),
      ['a', 'b'].map((name) => ({ name, path: join(path, 'backups', name) })),
    );
    const mem = Catalog.memory();
    try {
      assert.deepEqual(await mem.backupFiles(), []);
    } finally {
      await mem.close();
    }
  } finally {
    await cat.close();
    await rm(path, { recursive: true, force: true });
  }
});
test('catalog aliases observe reservation release and read-only helper protections', async () => {
  const path = await mkdtemp(join(tmpdir(), 'sparkles-reservation-alias-'));
  const cat = await Catalog.open(path);
  const alias = await Catalog.open(path, { readOnly: true });
  try {
    const ds = await cat.create('existing');
    cat.reserve('existing', 'restore', 'owner');
    assert.equal((await alias.info('existing')).reservedBy, 'owner');
    assert.throws(() => alias.reserve('new', 'clone', 'read-only'));
    await assert.rejects(
      alias.repositories.withFixed([{ name: 'local', type: 'fs', path: path + '-backups' }]),
    );
    await assert.rejects(
      alias.runPolicy({ name: 'nightly', repository: 'local', schedule: 'every 6h' }),
    );
    await assert.rejects(
      alias.applyRetention(
        { name: 'nightly', repository: 'local', schedule: 'every 6h' },
        { dryRun: false },
      ),
    );
    await cat.close();
    assert.equal((await alias.info('existing')).reservedBy, null);
    await ds.close();
  } finally {
    await cat.close();
    await alias.close();
    await rm(path, { recursive: true, force: true });
  }
});
test(
  'fixed registries, offline policy capture and retention preserve identities and counts',
  { timeout: 20000 },
  async () => {
    const path = await mkdtemp(join(tmpdir(), 'sparkles-policy-'));
    const cat = Catalog.memory();
    try {
      const ds = await cat.create('123', { kind: 'mem' });
      await ds.update('INSERT DATA { <urn:s> <urn:p> 1 }');
      const entries = await cat.repositories.withFixed([{ name: 'local', type: 'fs', path }]);
      assert.equal(entries[0].source, 'config');
      await assert.rejects(cat.repositories.remove('local'));
      await assert.rejects(
        cat.repositories.withFixed([
          { name: 'other', type: 'fs', path: path + '-unused' },
          { name: 'local', type: 'fs', path },
        ]),
      );
      assert.equal((await cat.repositories.list()).length, 1);
      const policy = {
        name: 'nightly',
        repository: 'local',
        schedule: 'every 6h',
        datasets: ['123'],
        nameTemplate: 'first-{dataset}',
        retention: { minCount: 0, maxCount: 1 },
      };
      const first = await cat.runPolicy(policy);
      assert.equal(first.result, 'ok');
      assert.equal(first.datasets[0].dataset, '123');
      assert.equal(first.datasets[0].backup, 'first-123');
      assert.equal(typeof first.datasets[0].addedBytes, 'bigint');
      assert.equal(typeof first.datasets[0].millis, 'bigint');
      const unrelated = await cat.create('other', { kind: 'mem' });
      await unrelated.transaction(
        async () => {
          const skipped = await cat.runPolicy({ ...policy, skipUnchanged: true }, { timeout: 500 });
          assert.equal(skipped.datasets[0].result, 'skipped');
        },
        { timeout: 1000 },
      );
      await unrelated.close();
      await assert.rejects(cat.runPolicy(policy, { dryRun: true }), UnsupportedError);
      await assert.rejects(cat.runPolicy(policy, { ifHead: 999n }), UnsupportedError);
      assert.throws(
        () => cat.applyRetention(policy, { dryRun: false, signal: AbortSignal.abort() }),
        UnsupportedError,
      );
      await ds.update('INSERT DATA { <urn:t> <urn:p> 2 }');
      const second = await cat.runPolicy({
        ...policy,
        nameTemplate: 'second-{dataset}',
        retention: { minCount: 2 },
      });
      assert.equal(second.result, 'ok');
      const preview = await cat.applyRetention(policy);
      assert.equal(preview.dryRun, true);
      assert.deepEqual(
        preview.delete.map((b) => b.name),
        ['first-123'],
      );
      assert.equal(preview.keep[0].dataset.name, '123');
      assert.equal(preview.keep[0].commit.seq, 2n);
      assert.equal(typeof preview.keep[0].logicalBytes, 'bigint');
      const repo = await cat.repositories.open('local');
      assert.equal((await repo.list()).length, 2);
      await cat.applyRetention(policy, { dryRun: false });
      assert.equal((await repo.list()).length, 1);
      await ds.transaction(async () => {
        await assert.rejects(cat.runPolicy(policy), ConflictError);
      });
      const aborted = AbortSignal.abort(new Error('policy aborted'));
      await assert.rejects(cat.runPolicy(policy, { signal: aborted }), /policy aborted/);
      await repo.close();
      await ds.close();
    } finally {
      await cat.close();
      await rm(path, { recursive: true, force: true });
    }
  },
);
test(
  'policy admission honors timeout and catalog close while a transaction owns the writer',
  { timeout: 10000 },
  async () => {
    const path = await mkdtemp(join(tmpdir(), 'sparkles-policy-admission-'));
    const cat = Catalog.memory();
    const ds = await cat.create('data', { kind: 'mem' });
    try {
      await cat.repositories.withFixed([{ name: 'local', type: 'fs', path }]);
      const policy = { name: 'nightly', repository: 'local', schedule: 'every 6h' };
      const transaction = await ds.beginTransaction();
      await assert.rejects(cat.runPolicy(policy, { timeout: 10 }), QueryTimeoutError);
      const pending = cat.runPolicy(policy);
      const observed = assert.rejects(pending);
      await new Promise((r) => setTimeout(r, 30));
      await cat.close();
      await observed;
      await transaction.rollback();
    } finally {
      await cat.close();
      await ds.close();
      await rm(path, { recursive: true, force: true });
    }
  },
);
