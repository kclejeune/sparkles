import { parentPort, workerData } from 'node:worker_threads';
import { Dataset, factory } from '../dist/index.js';
const ds = await Dataset.open(workerData.path);
if (workerData.mode === 'transaction') {
  const tx = await ds.beginTransaction();
  await tx.add(
    factory.quad(
      factory.namedNode('urn:lost'),
      factory.namedNode('urn:p'),
      factory.literal('rollback'),
    ),
  );
  parentPort.postMessage('ready');
  setInterval(() => {}, 1000);
} else {
  void ds.load(
    {
      async *[Symbol.asyncIterator]() {
        yield new TextEncoder().encode('<urn:lost> <urn:p> "rollback" .');
        parentPort.postMessage('ready');
        await new Promise(() => {});
      },
    },
    { format: 'nt' },
  );
  setInterval(() => {}, 1000);
}
