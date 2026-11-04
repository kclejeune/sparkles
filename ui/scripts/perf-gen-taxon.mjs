#!/usr/bin/env node
// Writes a synthetic ontology with the shape of NCBITaxon as N-Triples, for measuring the
// Workbench trees at the scale of a real taxonomy (ROADMAP item 725). The real NCBITaxon
// OBO release is not kept on any build or benchmark host, so this stands in for it: a tree
// of owl:Class nodes under one root with obo-style IRIs (NCBITaxon_<n>), a scientific name
// as rdfs:label, an exact synonym on some, a rank annotation on every node, a depth of up
// to 40 and a skewed fan-out, where a few nodes have 10,000 to 100,000 children and most
// have between none and 20. The same seed gives the same file.
//
//   node scripts/perf-gen-taxon.mjs [--classes 2600000] [--seed 725] [--out FILE]
//
// FILE defaults to ~/.cache/sparkles-ui-seed/taxon-synthetic-<classes>.nt. The script prints
// the node count, the depth and fan-out statistics, and the file size.

import { createWriteStream, mkdirSync, statSync, writeFileSync } from 'node:fs';
import { once } from 'node:events';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';

const arg = (name, dflt) => {
  const i = process.argv.indexOf(`--${name}`);
  return i > 0 ? process.argv[i + 1] : dflt;
};
const N = Number(arg('classes', '2600000'));
const SEED = Number(arg('seed', '725'));
const OUT = arg('out', join(homedir(), '.cache/sparkles-ui-seed', `taxon-synthetic-${N}.nt`));
const MAX_DEPTH = 40;
// --sidecar-only recomputes the JSON beside the file without writing the file
const SIDECAR_ONLY = process.argv.includes('--sidecar-only');

// mulberry32: a small deterministic generator
function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const rand = rng(SEED);

// --- the tree ------------------------------------------------------------------------
// ids 1..N; node 1 is the root. A frontier of nodes whose children are not yet drawn is
// popped from its head (breadth) most of the time and from its tail (depth) sometimes.
const parent = new Int32Array(N + 1);
const depth = new Uint8Array(N + 1);
const kids = new Uint32Array(N + 1);
const frontier = new Int32Array(N + 1);
let head = 0;
let tail = 0;
frontier[tail++] = 1;
let count = 1;
const HUBS = [100000, 70000, 50000, 40000, 30000, 25000, 20000, 20000, 15000, 12000, 10000, 10000];
// a smaller tree than 1 million classes gets proportionally smaller families, so that it
// keeps its shape (the largest is about 18,750 children at 300,000 classes)
const HUB_SCALE = N < 1_000_000 ? N / 1_600_000 : 1;
for (let i = 0; i < HUBS.length; i++) HUBS[i] = Math.round(HUBS[i] * HUB_SCALE);
let popped = 0;
let hubNext = 0;

function drawKids(d) {
  if (d >= MAX_DEPTH) return 0;
  // the first levels always branch, so that the tree cannot die out early
  if (d < 3) return 6 + Math.floor(rand() * 10);
  const r = rand();
  if (r < 0.04) return 1; // a chain
  if (r < 0.4) return 0;
  if (r < 0.97) return Math.min(20, 1 + Math.floor(-Math.log(1 - rand()) * 4));
  if (r < 0.995) return 21 + Math.floor(rand() * 280);
  return 300 + Math.floor(rand() * 2700);
}

while (head < tail && count < N) {
  // depth-first now and then, so that chains reach the depth of a real lineage
  const fromTail = rand() < 0.3 && tail - head > 1;
  const node = fromTail ? frontier[--tail] : frontier[head++];
  popped++;
  const d = depth[node];
  let want;
  if (d >= 3 && d <= 8 && hubNext < HUBS.length && popped > 200 * (hubNext + 1))
    want = HUBS[hubNext++];
  else want = drawKids(d);
  // the scale of the tree is kept by drawing bigger families at the bottom of the frontier
  if (want === 1 && rand() < 0.9) {
    // a chain: keep extending it in place while it continues
    let cur = node;
    while (count < N && depth[cur] < MAX_DEPTH) {
      const c = ++count;
      parent[c] = cur;
      depth[c] = depth[cur] + 1;
      kids[cur]++;
      cur = c;
      if (rand() < 0.12) break;
    }
    frontier[tail++] = cur;
    continue;
  }
  for (let i = 0; i < want && count < N; i++) {
    const c = ++count;
    parent[c] = node;
    depth[c] = d + 1;
    kids[node]++;
    frontier[tail++] = c;
  }
}

// --- the file --------------------------------------------------------------------------
if (count < N) throw new Error(`the tree stopped at ${count} of ${N} nodes`);
const OBO = 'http://purl.obolibrary.org/obo/';
const A = '<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>';
const SUB = '<http://www.w3.org/2000/01/rdf-schema#subClassOf>';
const LABEL = '<http://www.w3.org/2000/01/rdf-schema#label>';
const SYN = '<http://www.geneontology.org/formats/oboInOwl#hasExactSynonym>';
const RANK = `<${OBO}ncbitaxon#has_rank>`;
const CLASS = '<http://www.w3.org/2002/07/owl#Class>';
const ONT = '<http://www.w3.org/2002/07/owl#Ontology>';
const RANKS = [
  'superkingdom',
  'kingdom',
  'phylum',
  'class',
  'order',
  'family',
  'genus',
  'species',
  'subspecies',
];
const SYL = [
  'ba',
  'ce',
  'di',
  'fo',
  'gu',
  'ha',
  'ji',
  'ko',
  'la',
  'me',
  'ni',
  'or',
  'pu',
  'qua',
  're',
  'so',
  'tu',
  'vi',
  'wa',
  'xe',
  'yo',
  'zu',
  'bi',
  'ca',
  'do',
  'el',
  'fi',
  'ga',
];
const ENDS = ['us', 'a', 'um', 'ia', 'ella', 'ensis', 'ii', 'ae', 'ina', 'oides'];
function word(n) {
  let s = '';
  let x = n;
  for (let i = 0; i < 2 + (n % 3); i++) {
    s += SYL[x % SYL.length];
    x = Math.floor(x / SYL.length) + 7 * i + 1;
  }
  return s[0].toUpperCase() + s.slice(1) + ENDS[n % ENDS.length];
}
const iri = (n) => `<${OBO}NCBITaxon_${n}>`;

mkdirSync(dirname(OUT), { recursive: true });
const out = createWriteStream(SIDECAR_ONLY ? '/dev/null' : OUT);
let buf = '';
async function put(s) {
  buf += s;
  if (buf.length > 1 << 20) {
    if (!out.write(buf)) await once(out, 'drain');
    buf = '';
  }
}
await put(`<${OBO}ncbitaxon.owl> ${A} ${ONT} .\n`);
let syn = 0;
for (let n = 1; n <= N; n++) {
  const d = depth[n];
  const genus = word(parent[n] || n);
  const name = n === 1 ? 'root' : d >= 7 ? `${genus} ${word(n).toLowerCase()}` : word(n);
  let s = `${iri(n)} ${A} ${CLASS} .\n${iri(n)} ${LABEL} "${name}" .\n`;
  if (n > 1) s += `${iri(n)} ${SUB} ${iri(parent[n])} .\n`;
  s += `${iri(n)} ${RANK} <${OBO}NCBITaxon_${RANKS[Math.min(RANKS.length - 1, Math.max(0, d - 1))]}> .\n`;
  if (n % 7 === 0) {
    s += `${iri(n)} ${SYN} "${name.toLowerCase()} syn" .\n`;
    syn++;
  }
  await put(s);
}
out.end(buf);
await once(out, 'close');

// --- statistics -------------------------------------------------------------------------
let maxDepth = 0;
const sizes = [];
const hist = new Map();
for (let n = 1; n <= N; n++) {
  maxDepth = Math.max(maxDepth, depth[n]);
  if (kids[n] >= 1000) sizes.push(kids[n]);
  const b = kids[n] === 0 ? '0' : kids[n] <= 20 ? '1-20' : kids[n] < 1000 ? '21-999' : '1000+';
  hist.set(b, (hist.get(b) ?? 0) + 1);
}
sizes.sort((a, b) => b - a);
// the largest families by id, for the measurement scripts: the node with the most children,
// its IRI and its depth, written beside the file
const big = [];
for (let n = 1; n <= N; n++)
  if (kids[n] >= 10000) big.push({ id: n, children: kids[n], depth: depth[n] });
big.sort((a, b) => b.children - a.children);
const hubId = big[0].id;
let deepest = 1;
let firstChild = 0;
for (let n = 1; n <= N; n++) {
  if (depth[n] > depth[deepest]) deepest = n;
  if (!firstChild && parent[n] === hubId) firstChild = n;
}
const hubPath = (n) => {
  const out = [];
  for (let c = n; c; c = parent[c]) out.unshift(c);
  return out;
};
const at = (n) => `${OBO}NCBITaxon_${n}`;
writeFileSync(
  OUT.replace(/\.nt$/, '.json'),
  JSON.stringify(
    {
      root: at(1),
      hubs: big.map((b) => ({ ...b, iri: at(b.id) })),
      // the classes from the root down to the largest family, for opening it without selecting it
      hubPath: hubPath(hubId).map(at),
      // a class at the bottom of the tree, and one in the middle of the largest family
      deepest: { iri: at(deepest), depth: depth[deepest] },
      pastFirstPage: { iri: at(firstChild + Math.floor(big[0].children / 2)), parent: at(hubId) },
    },
    null,
    1,
  ),
);
console.log(
  JSON.stringify(
    {
      file: OUT,
      bytes: SIDECAR_ONLY ? 0 : statSync(OUT).size,
      classes: N,
      seed: SEED,
      maxDepth,
      fanOut: Object.fromEntries(hist),
      largestFamilies: sizes.slice(0, 15),
      synonyms: syn,
    },
    null,
    1,
  ),
);
