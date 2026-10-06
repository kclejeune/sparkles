import assert from 'node:assert/strict';
import {
  Dataset,
  Catalog,
  factory,
  checkIri,
  checkData,
  parseQuery,
  format,
  previewSchedule,
} from '@sparkles-rdf/engine';
import { SparklesClient } from '@sparkles-rdf/client';
const ds = Dataset.memory();
await ds.load('<urn:s> <urn:p> "installed" .', { format: 'nt' });
assert.equal(await ds.count(), 1n);
const r = await ds.select('SELECT ?o {?s ?p ?o}');
assert.equal((await r.toArray())[0].get('o').value, 'installed');
assert(
  await ds.has(
    factory.quad(
      factory.namedNode('urn:s'),
      factory.namedNode('urn:p'),
      factory.literal('installed'),
    ),
  ),
);
await ds.close();
const cat = Catalog.memory();
const d = await cat.create('one', { kind: 'mem' });
assert.equal((await cat.get('one')).id, d.id);
await cat.close();
assert.deepEqual((await checkIri('urn:installed')).errors, []);
assert.equal(await checkData('<urn:s> <urn:p> 1 .', 'ttl'), null);
assert.match(await parseQuery('ASK {}'), /ASK/);
assert.match((await format('# installed\nASK {}', 'sparql')).text, /# installed/);
assert.equal(
  (await previewSchedule('every 1h', { count: 1, after: '2026-01-01T00:00:00Z' })).length,
  1,
);
assert(SparklesClient);
console.log('Packed engine/client/common runtime smoke passed');
