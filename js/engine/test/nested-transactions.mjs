import assert from 'node:assert/strict';
import { Dataset, Catalog, ConflictError } from '../dist/index.js';
const a = Dataset.memory(),
  b = Dataset.memory();
try {
  await a.transaction(async () =>
    b.transaction(async () => {
      await assert.rejects(
        a.update('INSERT DATA {<urn:s> <urn:p> 1}', { timeout: 100 }),
        ConflictError,
      );
      await assert.rejects(a.cloneToMemory('blocked', { timeout: 100 }), ConflictError);
      await assert.rejects(a.compact({ timeout: 100 }), ConflictError);
    }),
  );
  assert.equal(await a.count(), 0n);
  const cat = Catalog.memory();
  try {
    const ds = await cat.create('a', { kind: 'mem' });
    const tx = await ds.beginTransaction();
    const copying = cat.clone('a', 'b');
    void copying.catch(() => {});
    await new Promise((resolve) => setTimeout(resolve, 20));
    await cat.close();
    await assert.rejects(copying);
    await tx.rollback();
  } finally {
    await cat.close();
  }
} finally {
  await a.close();
  await b.close();
}
