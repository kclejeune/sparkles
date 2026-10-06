// Run the same lifecycle under Node.js, Bun and Deno's Node-API compatibility.
import assert from 'node:assert/strict';
import {
  Dataset,
  Catalog,
  factory,
  checkIri,
  parseQuery,
  format,
  previewSchedule,
} from '../engine/dist/index.js';
const ds = Dataset.memory();
await ds.load('<urn:s> <urn:p> "runtime" .', { format: 'nt' });
assert.equal(await ds.count(), 1n);
const receipt = (await ds.update('INSERT DATA {<urn:a> <urn:p> 2}')).receipt;
assert.equal(receipt.commit.seq, 2n);
await ds.transaction(async (tx) => {
  await tx.add(
    factory.quad(factory.namedNode('urn:b'), factory.namedNode('urn:p'), factory.literal('value')),
  );
});
assert.equal(await ds.count(), 3n);
assert.equal((await (await ds.select('SELECT ?s {?s ?p ?o}')).toArray()).length, 3);
await ds.close();
assert.deepEqual((await checkIri('urn:runtime')).errors, []);
assert.match(
  await parseQuery('ASK { <s> <p> ?o }', { baseIri: 'https://example.org/' }),
  /https:\/\/example.org\/s/,
);
assert.match((await format('# runtime\nASK {}', 'sparql')).text, /# runtime/);
assert.equal(
  (await previewSchedule('every 1h', { count: 1, after: '2026-01-01T00:00:00Z' }))[0],
  '2026-01-01T01:00:00+00:00',
);
const cat = Catalog.memory();
const claim = cat.reserve('runtime', 'clone', 'smoke');
claim.close();
await cat.create('runtime', { kind: 'mem' });
await cat.close();
console.log('Node-API runtime lifecycle passed');
