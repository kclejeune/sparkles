import { parentPort, workerData } from 'node:worker_threads';
import { Dataset } from '../dist/index.js';
const ds = await Dataset.open(workerData.path);
try {
  await ds.update(`INSERT DATA { <urn:worker${await ds.count()}> <urn:p> "x" }`);
  parentPort.postMessage({ id: ds.id, count: String(await ds.count()) });
} finally {
  await ds.close();
}
