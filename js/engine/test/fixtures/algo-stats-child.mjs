import assert from 'node:assert/strict';
import { readFile, writeFile, rm } from 'node:fs/promises';
import { setTimeout as delay } from 'node:timers/promises';
import { join } from 'node:path';
import { Dataset } from '../../dist/index.js';

const [mode, dir] = process.argv.slice(2);
const slot = mode.startsWith('slot');
const timeout = mode.endsWith('timeout');
async function observed(name) {
  const end = Date.now() + 6000;
  for (;;) {
    try {
      return JSON.parse(await readFile(join(dir, name), 'utf8'));
    } catch (e) {
      if (e.code !== 'ENOENT') throw e;
      assert(Date.now() < end, `missing native acknowledgment ${name}`);
      await delay(2);
    }
  }
}
const ds = Dataset.memory();
try {
  await ds.update('INSERT DATA { <urn:a> <urn:p> <urn:b> }');
  await writeFile(join(dir, 'armed'), slot ? 'slot' : 'compute');
  const controller = new AbortController();
  const reason = new Error('explicit stats cancellation');
  const request = {
    algorithm: 'node2vec',
    projection: { predicates: ['urn:p'], direction: 'undirected' },
    params: {
      dimension: 1,
      walkLength: 70000,
      walksPerNode: 1,
      negativeSamples: 0,
      epochs: 1,
      concurrency: 1,
    },
  };
  const operation = ds.algorithms.stats(
    request,
    timeout ? { timeout: 1000 } : { signal: controller.signal },
  );
  // Attach before awaiting the acknowledgment, so an early rejection is never unhandled.
  const settled = operation.then(
    (value) => ({ value }),
    (error) => ({ error }),
  );
  const entry = await observed('entered.json');
  assert.equal(entry.mode, slot ? 'slot' : 'compute');
  if (timeout) {
    // The native work is paused at a proved boundary, not presumed to have entered
    // because a timer elapsed. Let the public timer signal its real native token.
    await delay(1050);
  } else controller.abort(reason);
  await writeFile(join(dir, 'release'), 'release');
  const result = await settled;
  assert(result.error, 'public operation must reject');
  if (timeout) assert.equal(result.error.name, 'QueryTimeoutError');
  else assert.equal(result.error, reason);
  const native = await observed('finished.json');
  assert.equal(native.acknowledged, true);
  assert(
    ['Cancelled', 'Timeout'].includes(native.result),
    `native work ignored task controls: ${JSON.stringify(native)}`,
  );
  assert(native.nativeOutcomes.some((v) => v === 'Cancelled' || v === 'Timeout'));
  assert.equal(entry.deadlineForwarded, true);
  assert.equal(native.algorithmEntered, !slot);
  assert.equal(native.holderReleasedEarly, false);
  assert.equal(native.slotReusable, true);
  await rm(join(dir, 'armed'));
  const stats = await ds.algorithms.stats({
    algorithm: 'wcc',
    projection: { predicates: ['urn:p'] },
  });
  assert.equal(stats.nodes, 2);
  assert.equal(stats.components, 1);
  console.log(JSON.stringify({ mode, entry, native, control: stats }));
} finally {
  await writeFile(join(dir, 'release'), 'cleanup');
  await ds.close();
}
