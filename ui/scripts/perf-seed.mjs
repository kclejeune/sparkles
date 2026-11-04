#!/usr/bin/env node
// Serves the datasets that measure the Workbench and Explore at scale (ROADMAP item 725)
// (`node scripts/perf-seed.mjs`). It loads three datasets into a persistent directory, once,
// and serves them on 127.0.0.1 until it is stopped with Ctrl+C or SIGTERM:
//
// - `chebi`, ChEBI lite (~/.cache/sparkles-ui-seed/chebi_lite.owl, about 200,000 classes)
// - `taxon`, a synthetic ontology with the shape of NCBITaxon, 2.6 million classes, from
//   `node scripts/perf-gen-taxon.mjs`
// - `taxon1m`, the same with 1 million classes, for the cases where 2.6 million do not open
// - `taxon300k`, with 300,000 classes and a largest family of 18,750, which the baseline opens
// - `hub`, a 100-ary tree and a resource with 20,000 neighbours, from `node scripts/perf-gen-hub.mjs`
//
// - `unesco`, the UNESCO Thesaurus of the design seed, for the thesaurus editor
//
// The generated files are written when they are missing. SPARKLES_BIN selects the binary
// (default ../target/release/sparkles), SPARKLES_UI_DIR a UI build to serve instead of the
// embedded one, SPARKLES_PERF_DATASETS the datasets to serve (default chebi,taxon,taxon1m,taxon300k,hub,unesco),
// SPARKLES_PERF_DIR the data directory (default ~/.cache/sparkles-ui-seed/perf-data, where a
// dataset that was loaded is kept), SPARKLES_SEED_PORT the port (default a free one) and
// SPARKLES_PERF_MEM the memory cap of the load and of the server (default 12G, applied with
// systemd-run when it exists). The script prints `ready <url>` once every dataset answers.

import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';

const ROOT = resolve(import.meta.dirname, '../..');
const BIN = process.env.SPARKLES_BIN ?? join(ROOT, 'target/release/sparkles');
const CACHE = join(homedir(), '.cache/sparkles-ui-seed');
const DIR = process.env.SPARKLES_PERF_DIR ?? join(CACHE, 'perf-data');
const WANT = (process.env.SPARKLES_PERF_DATASETS ?? 'chebi,taxon,taxon1m,taxon300k,hub,unesco')
  .split(',')
  .filter(Boolean);
const MEM = process.env.SPARKLES_PERF_MEM ?? '12G';

const SOURCES = {
  chebi: { file: join(CACHE, 'chebi_lite.owl'), gen: null },
  taxon: {
    file: join(CACHE, 'taxon-synthetic-2600000.nt'),
    gen: ['scripts/perf-gen-taxon.mjs'],
  },
  taxon1m: {
    file: join(CACHE, 'taxon-synthetic-1000000.nt'),
    gen: ['scripts/perf-gen-taxon.mjs', '--classes', '1000000'],
  },
  taxon300k: {
    file: join(CACHE, 'taxon-synthetic-300000.nt'),
    gen: ['scripts/perf-gen-taxon.mjs', '--classes', '300000'],
  },
  hub: { file: join(CACHE, 'hub-synthetic.nt'), gen: ['scripts/perf-gen-hub.mjs'] },
  unesco: { file: join(CACHE, 'unesco-thesaurus.ttl'), gen: null },
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const scoped = (cmd, args) =>
  spawnSync('which', ['systemd-run']).status === 0
    ? ['systemd-run', ['--user', '--scope', '-q', '-p', `MemoryMax=${MEM}`, cmd, ...args]]
    : [cmd, args];

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

function load(ds) {
  const marker = join(DIR, ds, '.perf-loaded');
  if (existsSync(marker)) return;
  const src = SOURCES[ds];
  if (!existsSync(src.file)) {
    if (!src.gen) throw new Error(`${src.file} is missing`);
    const r = spawnSync('node', [join(ROOT, 'ui', src.gen[0]), ...src.gen.slice(1)], {
      stdio: 'inherit',
    });
    if (r.status !== 0) throw new Error(`${src.gen[0]} failed`);
  }
  rmSync(join(DIR, ds), { recursive: true, force: true });
  const t = Date.now();
  const [cmd, args] = scoped(BIN, ['load', '--loc', join(DIR, ds), src.file]);
  const r = spawnSync(cmd, args, { encoding: 'utf8', maxBuffer: 1 << 28 });
  if (r.status !== 0) throw new Error(`sparkles load ${src.file} failed:\n${r.stderr}`);
  writeFileSync(marker, `${new Date().toISOString()}\n`);
  console.log(`loaded ${src.file} into ${ds} in ${((Date.now() - t) / 1000).toFixed(1)} s`);
}

async function main() {
  if (!existsSync(BIN)) throw new Error(`no Sparkles binary at ${BIN}`);
  mkdirSync(DIR, { recursive: true });
  for (const ds of WANT) load(ds);
  const port = Number(process.env.SPARKLES_SEED_PORT ?? (await freePort()));
  const url = `http://127.0.0.1:${port}`;
  const args = [
    'serve',
    '--data',
    join(DIR, 'data'),
    '--host',
    '127.0.0.1',
    '--port',
    String(port),
  ];
  for (const ds of WANT) args.push('--loc', `${ds}=${join(DIR, ds)}`);
  const [cmd, full] = scoped(BIN, args);
  const child = spawn(cmd, full, { stdio: 'inherit', env: process.env });
  const stop = () => child.kill('SIGTERM');
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
  child.once('exit', () => process.exit(0));
  for (let i = 0; ; i++) {
    try {
      if ((await fetch(`${url}/$/ping`)).ok) break;
    } catch {
      // not listening yet
    }
    if (i > 1200) throw new Error('sparkles serve did not answer within 120 s');
    await sleep(100);
  }
  for (const ds of WANT)
    for (let i = 0; ; i++) {
      const r = await fetch(`${url}/$/ready/${ds}`);
      if (r.ok) break;
      if (i > 6000) throw new Error(`dataset ${ds} did not open within 10 min`);
      await sleep(100);
    }
  console.log(`ready ${url}`);
}

main().catch((e) => {
  console.error(e);
  process.exitCode = 1;
  process.emit('SIGTERM');
});
