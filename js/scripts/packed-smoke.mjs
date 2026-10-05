import assert from 'node:assert/strict';
import { Dataset, Catalog, factory } from '@sparkles-rdf/engine';
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
assert(SparklesClient);
console.log('Packed engine/client/common runtime smoke passed');
