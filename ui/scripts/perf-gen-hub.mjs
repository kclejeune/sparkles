#!/usr/bin/env node
// Writes the instance graph that measures Explore at scale (ROADMAP item 725) as N-Triples.
//
// - `ex:tree` is the root of a 90-ary tree of `--tree` nodes (default 25,000). Each node
//   links to its 90 children by one of six predicates, fifteen to a predicate, so expanding a node draws 90
//   neighbours, the way Explore reads at most 100 edges each way, and expanding the root's
//   children reaches about 8,000 nodes. That gives the 1,000, 5,000 and 20,000 node cases.
// - `ex:big` is a resource with `--big` (default 20,000) outgoing `ex:contains` edges, for
//   the fan-out that a group node folds. Every second contained node also links back to it
//   by `ex:partOf`, so the hub has both directions.
//
// Every node has an rdfs:label ("Node 123") and one of twelve classes, and a few random
// links between nodes of the same family so that the picture has cycles.
//
//   node scripts/perf-gen-hub.mjs [--tree 25000] [--big 20000] [--out FILE]

import { createWriteStream, mkdirSync, statSync } from 'node:fs';
import { once } from 'node:events';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

const arg = (name, dflt) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : dflt;
};
const TREE = Number(arg('tree', '25000'));
const BIG = Number(arg('big', '20000'));
const OUT = arg('out', join(homedir(), '.cache/sparkles-ui-seed', 'hub-synthetic.nt'));
const EX = 'http://example.org/perf#';
const A = '<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>';
const LABEL = '<http://www.w3.org/2000/01/rdf-schema#label>';
// 90 children by 6 predicates is 15 edges a predicate, under the 25 at which the changed Explore
// folds a predicate into a group node, and 90 edges, under its 100 inline edges, so that an
// expansion draws its neighbours the same way in the baseline and in the changed page
const FAN = 90;
const PREDS = ['contains', 'relatedTo', 'derivedFrom', 'partOf', 'seeAlso', 'precedes'];
const iri = (s) => `<${EX}${s}>`;

mkdirSync(dirname(OUT), { recursive: true });
const out = createWriteStream(OUT);
let buf = '';
async function put(s) {
  buf += s;
  if (buf.length > 1 << 20) {
    if (!out.write(buf)) await once(out, 'drain');
    buf = '';
  }
}
const node = async (id, label, cls) =>
  put(`${iri(id)} ${A} ${iri(`Class${cls % 12}`)} .\n${iri(id)} ${LABEL} "${label}" .\n`);

for (let c = 0; c < 12; c++) await put(`${iri(`Class${c}`)} ${LABEL} "Class ${c}" .\n`);
await node('tree', 'Tree root', 0);
for (let i = 1; i < TREE; i++) {
  const parent = i === 1 ? 0 : Math.floor((i - 1) / FAN);
  const pid = parent === 0 ? 'tree' : `t${parent}`;
  const id = `t${i}`;
  await node(id, `Node ${i}`, i);
  await put(`${iri(pid)} ${iri(PREDS[i % PREDS.length])} ${iri(id)} .\n`);
  if (i % 50 === 0 && i > 200)
    await put(`${iri(id)} ${iri('relatedTo')} ${iri(`t${i - 100}`)} .\n`);
}
await node('big', 'Big hub', 3);
for (let i = 0; i < BIG; i++) {
  await node(`b${i}`, `Contained ${i}`, i);
  await put(`${iri('big')} ${iri('contains')} ${iri(`b${i}`)} .\n`);
  if (i % 2) await put(`${iri(`b${i}`)} ${iri('partOf')} ${iri('big')} .\n`);
}
out.end(buf);
await once(out, 'close');
console.log(JSON.stringify({ file: OUT, bytes: statSync(OUT).size, tree: TREE, big: BIG }));
