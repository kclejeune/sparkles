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
// one, and SPARKLES_SEED_PORT the port (default assigned by the kernel). SPARKLES_SEED_SKIP=go leaves
// out the slow GO load. SPARKLES_SEED_ASK=1 also starts the stand-in model provider of
// scripts/ask-stub.mjs and turns on the `demo` dataset's assistant, so that the Ask thread
// can be checked against a real server without a paid model. The script prints
// `ready <url>` once every dataset answers.

import { spawnSync } from 'node:child_process';
import {
  createWriteStream,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { startAskStub, stubModelConfig } from './ask-stub.mjs';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { runSeedServer, waitForReady } from './server-process.mjs';

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
  let stub;
  try {
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

    const args = [
      'serve',
      '--data',
      join(dir, 'data'),
      '--host',
      '127.0.0.1',
      '--port',
      process.env.SPARKLES_SEED_PORT ?? '0',
    ];
    for (const ds of datasets) args.push('--loc', `${ds}=${join(dir, ds)}`);
    args.push('--text', 'demo', '--geo', 'demo');
    if (ASK) stub = await startAskStub();
    if (stub) {
      const cfg = join(dir, 'models.json');
      writeFileSync(cfg, JSON.stringify(stubModelConfig(stub.url)));
      args.push('--model-config', cfg, '--outbound-allow-private');
    }
    await runSeedServer(BIN, args, join(dir, 'server.log'), 60_000, async (child, url) => {
      for (const ds of datasets) await waitForReady(child, `${url}/$/ready/${ds}`, 300_000);
      for (const [name, def] of Object.entries(STORED))
        await call(url, 'PUT', `/$/queries/demo/${name}`, def);
      for (const name of ['draft-places', 'review-2026'])
        await call(url, 'POST', '/$/branches/demo', { name, note: `Seeded branch ${name}` });
      if (stub)
        await call(url, 'PUT', '/$/assistant/demo', {
          enabled: true,
          send: 'rows',
          historyDays: 30,
        });
    });
  } finally {
    stub?.close();
    rmSync(dir, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error(e);
  process.exitCode = 1;
});
