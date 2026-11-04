#!/usr/bin/env node
// Serves realistic data for reviewing the web UI's layout by hand or with Playwright
// (`node scripts/seed-design.mjs`). It loads three datasets into a temporary directory and
// serves them on 127.0.0.1 until it is stopped with Ctrl+C or SIGTERM:
//
// - `go`, the Gene Ontology's go-basic release, for the ontology pages
// - `unesco`, the UNESCO Thesaurus in SKOS, for the thesaurus page
// - `demo`, the demo dataset of docs/demo, with stored queries and two branches
//
// The ontology and the thesaurus are downloaded once into SPARKLES_SEED_CACHE (default
// ~/.cache/sparkles-ui-seed). SPARKLES_BIN selects the binary (default
// ../target/debug/sparkles), SPARKLES_UI_DIR a UI build to serve instead of the embedded
// one, and SPARKLES_SEED_PORT the port (default a free one). SPARKLES_SEED_SKIP=go leaves
// out the slow GO load. SPARKLES_SEED_ASK=1 also starts the stand-in model provider of
// scripts/ask-stub.mjs and turns on the `demo` dataset's assistant, so that the Ask thread
// can be checked against a real server without a paid model. The script prints
// `ready <url>` once every dataset answers.

import { spawn, spawnSync } from 'node:child_process';
import {
  createWriteStream,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { createServer } from 'node:net';
import { startAskStub, stubModelConfig } from './ask-stub.mjs';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';

const ROOT = resolve(import.meta.dirname, '../..');
const BIN = process.env.SPARKLES_BIN ?? join(ROOT, 'target/debug/sparkles');
const CACHE = process.env.SPARKLES_SEED_CACHE ?? join(homedir(), '.cache/sparkles-ui-seed');
const SKIP = new Set((process.env.SPARKLES_SEED_SKIP ?? '').split(',').filter(Boolean));
const ASK = process.env.SPARKLES_SEED_ASK === '1';

const SOURCES = {
  go: { file: 'go-basic.owl', url: 'https://current.geneontology.org/ontology/go-basic.owl' },
  unesco: {
    file: 'unesco-thesaurus.ttl',
    url: 'https://vocabularies.unesco.org/exports/thesaurus/latest/unesco-thesaurus.ttl',
  },
};

const STORED = {
  'people-by-city': {
    description: 'People who live in a city, by name',
    query:
      'PREFIX ex: <http://example.org/ns#>\nPREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\n' +
      'SELECT ?person ?name WHERE { ?person ex:livesIn ?city ; rdfs:label ?name } ORDER BY ?name',
    parameters: { city: { type: 'iri', description: 'The city' } },
    results: 'json',
  },
  'class-sizes': {
    description: 'The number of instances of each class',
    query:
      'SELECT ?class (COUNT(?s) AS ?n) WHERE { ?s a ?class } GROUP BY ?class ORDER BY DESC(?n)',
    results: 'json',
  },
  'labels-matching': {
    description: 'Entities whose label contains a text',
    query:
      'PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\n' +
      'SELECT ?s ?label WHERE { ?s rdfs:label ?label FILTER(CONTAINS(LCASE(?label), LCASE(?text))) } LIMIT 100',
    parameters: { text: { type: 'string', default: 'a', description: 'Text to find' } },
    results: 'json',
  },
};

function freePort() {
  return new Promise((ok, fail) => {
    const s = createServer();
    s.once('error', fail);
    s.listen(0, '127.0.0.1', () => {
      const { port } = s.address();
      s.close(() => ok(port));
    });
  });
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function download(name) {
  const { file, url } = SOURCES[name];
  const path = join(CACHE, file);
  if (existsSync(path)) return path;
  mkdirSync(CACHE, { recursive: true });
  console.log(`downloading ${url}`);
  const r = await fetch(url);
  if (!r.ok || !r.body) throw new Error(`${url} answered ${r.status}`);
  await pipeline(Readable.fromWeb(r.body), createWriteStream(path + '.part'));
  spawnSync('mv', [path + '.part', path]);
  return path;
}

function load(dir, ds, file) {
  const t = Date.now();
  const r = spawnSync(BIN, ['load', '--loc', join(dir, ds), file], { encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`sparkles load ${file} failed:\n${r.stderr}`);
  console.log(`loaded ${file} into ${ds} in ${((Date.now() - t) / 1000).toFixed(1)} s`);
}

async function call(url, method, path, body) {
  const r = await fetch(url + path, {
    method,
    headers: body ? { 'content-type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!r.ok) throw new Error(`${method} ${path} answered ${r.status}: ${await r.text()}`);
}

async function main() {
  if (!existsSync(BIN)) throw new Error(`no Sparkles binary at ${BIN}`);
  const dir = mkdtempSync(join(tmpdir(), 'sparkles-design-'));
  const datasets = ['demo'];
  const demo = join(ROOT, 'docs/demo');
  const files = readdirSync(demo)
    .filter((f) => f.endsWith('.ttl'))
    .sort((a, b) => Number(b === 'vocab.ttl') - Number(a === 'vocab.ttl') || a.localeCompare(b));
  for (const f of files) load(dir, 'demo', join(demo, f));
  for (const ds of ['unesco', 'go']) {
    if (SKIP.has(ds)) continue;
    load(dir, ds, await download(ds));
    datasets.push(ds);
  }

  const port = Number(process.env.SPARKLES_SEED_PORT ?? (await freePort()));
  const url = `http://127.0.0.1:${port}`;
  const args = [
    'serve',
    '--data',
    join(dir, 'data'),
    '--host',
    '127.0.0.1',
    '--port',
    String(port),
  ];
  for (const ds of datasets) args.push('--loc', `${ds}=${join(dir, ds)}`);
  args.push('--text', 'demo', '--geo', 'demo');
  const stub = ASK ? await startAskStub() : null;
  if (stub) {
    const cfg = join(dir, 'models.json');
    writeFileSync(cfg, JSON.stringify(stubModelConfig(stub.url)));
    args.push('--model-config', cfg, '--outbound-allow-private');
  }
  const child = spawn(BIN, args, { stdio: 'inherit' });
  const stop = () => {
    child.kill('SIGTERM');
    stub?.close();
  };
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
  child.once('exit', () => {
    rmSync(dir, { recursive: true, force: true });
    process.exit(0);
  });

  for (let i = 0; ; i++) {
    try {
      if ((await fetch(`${url}/$/ping`)).ok) break;
    } catch {
      // not listening yet
    }
    if (i > 600) throw new Error('sparkles serve did not answer within 60 s');
    await sleep(100);
  }
  for (const ds of datasets) {
    for (let i = 0; ; i++) {
      const r = await fetch(`${url}/$/ready/${ds}`);
      if (r.ok) break;
      if (i > 3000) throw new Error(`dataset ${ds} did not open within 5 min`);
      await sleep(100);
    }
  }
  for (const [name, def] of Object.entries(STORED))
    await call(url, 'PUT', `/$/queries/demo/${name}`, def);
  for (const name of ['draft-places', 'review-2026'])
    await call(url, 'POST', '/$/branches/demo', { name, note: `Seeded branch ${name}` });
  if (stub)
    await call(url, 'PUT', '/$/assistant/demo', { enabled: true, send: 'rows', historyDays: 30 });
  console.log(`ready ${url}`);
}

main().catch((e) => {
  console.error(e);
  process.exitCode = 1;
  process.emit('SIGTERM');
});
