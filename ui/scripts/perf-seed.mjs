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
// - `hub`, a 90-ary tree and a resource with 20,000 neighbours, from `node scripts/perf-gen-hub.mjs`
//
// - `unesco`, the UNESCO Thesaurus of the design seed, for the thesaurus editor
//
// The generated files are written when they are missing. SPARKLES_BIN selects the binary
// (default ../target/release/sparkles), SPARKLES_UI_DIR a UI build to serve instead of the
// embedded one, SPARKLES_PERF_DATASETS the datasets to serve (default chebi,taxon,taxon1m,taxon300k,hub,unesco),
// SPARKLES_PERF_DIR the data directory (default ~/.cache/sparkles-ui-seed/perf-data, where a
// dataset that was loaded is kept), SPARKLES_SEED_PORT the port (default assigned by the kernel) and
// SPARKLES_PERF_MEM the memory cap of the load and of the server (default 12G, applied with
// systemd-run when it exists). The script prints `ready <url>` once every dataset answers.

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { runSeedServer, waitForReady } from './server-process.mjs';

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

const scoped = (cmd, args) =>
  spawnSync('which', ['systemd-run']).status === 0
    ? ['systemd-run', ['--user', '--scope', '-q', '-p', `MemoryMax=${MEM}`, cmd, ...args]]
    : [cmd, args];

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
  const args = [
    'serve',
    '--data',
    join(DIR, 'data'),
    '--host',
    '127.0.0.1',
    '--port',
    process.env.SPARKLES_SEED_PORT ?? '0',
  ];
  for (const ds of WANT) args.push('--loc', `${ds}=${join(DIR, ds)}`);
  const [cmd, full] = scoped(BIN, args);
  const run = mkdtempSync(join(tmpdir(), 'sparkles-perf-seed-'));
  try {
    await runSeedServer(cmd, full, join(run, 'server.log'), 120_000, async (child, url) => {
      for (const ds of WANT) await waitForReady(child, `${url}/$/ready/${ds}`, 600_000);
    });
  } finally {
    rmSync(run, { recursive: true, force: true });
  }
}

main().catch((e) => {
  console.error(e);
  process.exitCode = 1;
});
