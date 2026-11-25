// Serves the demo dataset for the README's screenshots: `sparkles load` puts docs/demo/*.ttl
// into a database in a temporary directory, and `sparkles serve` serves it as `demo` on a
// kernel-assigned port on 127.0.0.1, with the spatial index and full-text search on. Once both
// indexes are ready, the spec gets the server's URL in SPARKLES_SCREENSHOTS_URL. The
// returned function stops the server (its own process, by PID) and removes the directory.
//
// SPARKLES_BIN selects the binary (default: ../target/release/sparkles, which embeds the UI).

import type { FullConfig } from '@playwright/test';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { startServer, stopServer } from '../server-process';

const ROOT = resolve(import.meta.dirname, '../../..');
export const DEMO = join(ROOT, 'docs/demo');
export const DATASET = 'demo';

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Polls `GET url` until `ready` holds for its JSON body. */
async function poll(url: string, what: string, ready: (s: Record<string, unknown>) => boolean) {
  for (let i = 0; i < 300; i++) {
    const r = await fetch(url);
    if (r.ok) {
      const s = await r.json();
      if (ready(s)) return;
      if (i === 299) throw new Error(`${what} did not get ready: ${JSON.stringify(s)}`);
    }
    await sleep(100);
  }
  throw new Error(`${what} did not answer`);
}

export default async function globalSetup(_config: FullConfig) {
  const bin = process.env.SPARKLES_BIN ?? join(ROOT, 'target/release/sparkles');
  if (!existsSync(bin))
    throw new Error(`no Sparkles binary at ${bin}: run \`mise run build\` or set SPARKLES_BIN`);
  const dir = mkdtempSync(join(tmpdir(), 'sparkles-screenshots-'));
  const log = join(dir, 'server.log');

  // one commit per file, for the dataset page's history: the vocabulary first
  const files = readdirSync(DEMO)
    .filter((f) => f.endsWith('.ttl'))
    .sort((a, b) => Number(b === 'vocab.ttl') - Number(a === 'vocab.ttl') || a.localeCompare(b));
  for (const f of files) {
    const loaded = spawnSync(bin, ['load', '--loc', join(dir, DATASET), join(DEMO, f)], {
      encoding: 'utf8',
    });
    if (loaded.status !== 0) throw new Error(`sparkles load ${f} failed:\n${loaded.stderr}`);
  }

  const { child, url } = await startServer(
    bin,
    [
      'serve',
      '--data',
      join(dir, 'data'),
      '--host',
      '127.0.0.1',
      '--port',
      '0',
      '--loc',
      `${DATASET}=${join(dir, DATASET)}`,
      '--geo',
      DATASET,
      '--text',
      DATASET,
    ],
    log,
  );
  const kill = () => child.kill('SIGKILL');
  process.once('exit', kill);

  try {
    await poll(`${url}/$/geo/${DATASET}`, 'the spatial index', (s) => s.state === 'ready');
    await poll(
      `${url}/$/text/${DATASET}`,
      'the full-text index',
      (s) => s.state === 'ready' && s.seq === s.storeSeq,
    );
  } catch (e) {
    process.off('exit', kill);
    await stopServer(child);
    throw new Error(`${e instanceof Error ? e.message : e}; server log ${log}`, { cause: e });
  }
  process.env.SPARKLES_SCREENSHOTS_URL = url;

  return async () => {
    process.off('exit', kill);
    await stopServer(child);
    rmSync(dir, { recursive: true, force: true });
  };
}
