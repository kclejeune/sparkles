import { parentPort, workerData } from 'node:worker_threads';
import { native } from '../dist/native.js';
import { TermCache } from '../dist/terms.js';
const ds = await native.NativeDataset.open(workerData.path, '{}');
const cancel = new native.Cancellation();
if (workerData.mode === 'beginQueued' || workerData.mode === 'loadQueued') {
  void (
    workerData.mode === 'beginQueued'
      ? ds.begin('{}', cancel)
      : ds.loadStart('{"format":"nt"}', cancel)
  ).catch(() => {});
  setTimeout(() => parentPort.postMessage('ready'), 120);
} else if (workerData.mode === 'txApply' || workerData.mode === 'txEnd') {
  const tx = await ds.begin('{}', cancel);
  const quad = (i) => ({
    termType: 'Quad',
    value: '',
    subject: { termType: 'NamedNode', value: `urn:transient-${i}` },
    predicate: { termType: 'NamedNode', value: 'urn:p' },
    object: {
      termType: 'Literal',
      value: 'x',
      language: '',
      datatype: { value: 'http://www.w3.org/2001/XMLSchema#string' },
    },
    graph: { termType: 'DefaultGraph', value: '' },
  });
  const terms = new TermCache();
  const write = (quads) => {
    const { text, data } = terms.encode(quads);
    return Promise.resolve(tx.write(true, text, data));
  };
  if (workerData.mode === 'txApply')
    void write(Array.from({ length: 4096 }, (_, i) => quad(i))).catch(() => {});
  else {
    await write([quad('commit')]);
    void tx.end(true).catch(() => {});
  }
  parentPort.postMessage('ready');
} else if (workerData.mode === 'uploadPush') {
  const upload = await ds.loadStart('{"format":"nt"}', cancel);
  await upload.push(Buffer.from('<urn:s> <urn:p> "'));
  for (let i = 0; i < 64; i++) void upload.push(Buffer.alloc(65536, 120)).catch(() => {});
  parentPort.postMessage('ready');
} else if (workerData.mode === 'uploadFinish') {
  const upload = await ds.loadStart('{"format":"nt"}', cancel);
  void upload.finish().catch(() => {});
  parentPort.postMessage('ready');
} else if (workerData.mode === 'waitCommit') {
  void ds.waitCommit('18446744073709551615', cancel).catch(() => {});
  setTimeout(() => parentPort.postMessage('ready'), 120);
} else {
  const stream = ds.dumpStream('{"format":"nq"}', cancel);
  void stream.next().catch(() => {});
  parentPort.postMessage('ready');
}
