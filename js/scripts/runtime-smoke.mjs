// Run the same lifecycle under Node.js, Bun and Deno's Node-API compatibility.
import assert from 'node:assert/strict';
import { Dataset, factory } from '../engine/dist/index.js';
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
console.log('Node-API runtime lifecycle passed');
