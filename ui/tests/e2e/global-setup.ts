// Starts a real `sparkles serve` for the end-to-end tests: a temporary data directory, a
// free port on 127.0.0.1 and an auth configuration with one local user and one static API
// token. It loads a small dataset (labels, comments for full-text search, vectors for
// "Similar"), signs the user in, and hands the details to the tests through environment
// variables (inherited by the workers). A second server without auth (the default
// `sparkles serve`) starts with only an in-memory dataset the operator declares, a
// settings file that also locks a model provider's endpoint, a model configuration with
// a declared key, and a backup config that lets its API register repositories under a
// temporary directory. The returned function is the global teardown.
//
// SPARKLES_BIN selects the binary (default: ../target/debug/sparkles, which serves ui/build
// from disk); SPARKLES_E2E_KEEP=1 keeps the data directory and server log;
// SPARKLES_E2E_PORT=N runs the servers on ports N and N + 1 instead of free ones.

import { request, type FullConfig } from '@playwright/test';
import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import {
  closeSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import {
  DATASET,
  DATA,
  DECLARED_DATASET,
  DECLARED_KEY,
  MODELS,
  PASSWORD,
  SETTINGS,
  USER,
} from './data';

const UI_DIR = resolve(import.meta.dirname, '../..');

function binary(): string {
  const bin = process.env.SPARKLES_BIN ?? resolve(UI_DIR, '../target/debug/sparkles');
  if (!existsSync(bin))
    throw new Error(
      `no Sparkles binary at ${bin}: run \`cargo build -p sparkles-server\` or set SPARKLES_BIN`,
    );
  return bin;
}

/** A port that was free a moment ago (the server is retried on another if it is taken). */
function freePort(): Promise<number> {
  return new Promise((ok, fail) => {
    const s = createServer();
    s.once('error', fail);
    s.listen(0, '127.0.0.1', () => {
      const addr = s.address();
      s.close(() =>
        addr && typeof addr === 'object' ? ok(addr.port) : fail(new Error('no port')),
      );
    });
  });
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** SIGTERM (a graceful drain), then SIGKILL if it has not exited after `ms`. */
async function stop(child: ChildProcess, ms = 10_000) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise<void>((r) => child.once('exit', () => r()));
  child.kill('SIGTERM');
  const timer = setTimeout(() => child.kill('SIGKILL'), ms);
  await exited;
  clearTimeout(timer);
}

/** Start the server; resolves once `/$/ping` answers, rejects if the process exits first. */
async function start(bin: string, args: string[], log: string) {
  const out = openSync(log, 'a');
  const child = spawn(bin, args, {
    stdio: ['ignore', out, out],
    env: { ...process.env, RUST_LOG: process.env.RUST_LOG ?? 'info' },
  });
  closeSync(out);
  let exit: string | null = null;
  child.once('exit', (code, signal) => (exit = `exited with ${signal ?? code}`));
  const url = `http://${args[args.indexOf('--host') + 1]}:${args[args.indexOf('--port') + 1]}`;
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    if (exit) return { child, url, error: exit };
    try {
      if ((await fetch(`${url}/$/ping`)).ok) return { child, url, error: null };
    } catch {
      // not listening yet
    }
    await sleep(100);
  }
  await stop(child);
  return { child, url, error: 'did not answer /$/ping within 30 s' };
}

async function check(r: Response, what: string) {
  if (!r.ok) throw new Error(`${what}: ${r.status} ${await r.text()}`);
  return r;
}

/** `SPARKLES_E2E_PORT`: the first server's port, and the next one the second server's. */
const fixedPort = process.env.SPARKLES_E2E_PORT ? Number(process.env.SPARKLES_E2E_PORT) : null;

/**
 * `sparkles serve` on 127.0.0.1 with these extra arguments, on a free port, or on
 * `SPARKLES_E2E_PORT` plus `offset` when that is set.
 */
async function serve(bin: string, data: string, log: string, extra: string[], offset = 0) {
  let server: Awaited<ReturnType<typeof start>> | null = null;
  for (let attempt = 0; attempt < (fixedPort == null ? 3 : 1); attempt++) {
    const port = fixedPort == null ? await freePort() : fixedPort + offset;
    // --host is explicit: the tests must not listen on every interface
    server = await start(
      bin,
      ['serve', '--data', data, '--host', '127.0.0.1', '--port', String(port), ...extra],
      log,
    );
    if (!server.error) break;
    // retried only when another process took the port in the meantime
    if (!/address already in use|binding/i.test(readFileSync(log, 'utf8'))) break;
  }
  if (!server || server.error) {
    const tail = readFileSync(log, 'utf8').split('\n').slice(-30).join('\n');
    throw new Error(`sparkles serve ${server?.error}; log ${log}:\n${tail}`);
  }
  return server;
}

export default async function globalSetup(_config: FullConfig) {
  const bin = binary();
  const dir = mkdtempSync(join(tmpdir(), 'sparkles-e2e-'));
  const log = join(dir, 'server.log');

  // A static token (spk_ + 43 base64url characters), stored in the config as its sha256.
  const token = `spk_${randomBytes(32).toString('base64url')}`;
  const hashed = spawnSync(bin, ['auth', 'hash'], { input: `${PASSWORD}\n`, encoding: 'utf8' });
  if (hashed.status !== 0) throw new Error(`sparkles auth hash failed: ${hashed.stderr}`);
  const authConfig = join(dir, 'auth.toml');
  writeFileSync(
    authConfig,
    `version = 1

[[users]]
name = "${USER}"
password = "${hashed.stdout.trim()}"
datasets = { ${DATASET} = "write" }

[[tokens]]
name = "e2e-setup"
hash = "sha256:${createHash('sha256').update(token).digest('hex')}"
server = ["server-admin"]
`,
    { mode: 0o600 },
  );

  const { child, url } = await serve(bin, join(dir, 'data'), log, ['--auth-config', authConfig]);
  // never leave the servers running, even if the runner dies without a teardown
  let open: ChildProcess | null = null;
  const kill = () => {
    child.kill('SIGKILL');
    open?.kill('SIGKILL');
  };
  process.once('exit', kill);

  try {
    // the dataset: created, loaded in two commits, full-text indexed (as the setup token)
    const auth = { Authorization: `Bearer ${token}` };
    await check(
      await fetch(`${url}/$/datasets`, {
        method: 'POST',
        headers: { ...auth, 'Content-Type': 'application/json' },
        body: JSON.stringify({ dbName: DATASET, dbType: 'persistent' }),
      }),
      'creating the dataset',
    );
    for (const [i, part] of DATA.entries())
      await check(
        await fetch(`${url}/${DATASET}/data`, {
          method: 'POST',
          headers: { ...auth, 'Content-Type': 'text/turtle' },
          body: part,
        }),
        `loading part ${i + 1}`,
      );
    await check(
      await fetch(`${url}/$/text/${DATASET}`, { method: 'PUT', headers: auth }),
      'enabling full-text search',
    );
    for (let i = 0; ; i++) {
      const s = await (await fetch(`${url}/$/text/${DATASET}`, { headers: auth })).json();
      if (s.state === 'ready' && s.seq === s.storeSeq && s.docs > 0) break;
      if (i > 300) throw new Error(`the full-text index did not get ready: ${JSON.stringify(s)}`);
      await sleep(100);
    }

    // the user's session cookie, for every test that does not sign in itself
    const state = join(dir, 'storage-state.json');
    const ctx = await request.newContext({ baseURL: url });
    const login = await ctx.post('/$/auth/login', { data: { user: USER, password: PASSWORD } });
    if (!login.ok()) throw new Error(`signing in: ${login.status()} ${await login.text()}`);
    await ctx.storageState({ path: state });
    await ctx.dispose();

    process.env.SPARKLES_E2E_URL = url;
    process.env.SPARKLES_E2E_TOKEN = token;
    process.env.SPARKLES_E2E_STATE = state;

    // backup repositories registered through its API must lie under repos/ (the config
    // file's own directory is off limits to them)
    const repos = join(dir, 'repos');
    mkdirSync(repos);
    mkdirSync(join(dir, 'conf'));
    const backupConfig = join(dir, 'conf', 'backup.toml');
    writeFileSync(backupConfig, `version = 1\n\n[api]\nfs_roots = [${JSON.stringify(repos)}]\n`, {
      mode: 0o600,
    });
    // declared settings (C19) for the dataset the settings tests create, and a dataset
    // the operator declares, which the UI does not offer to delete
    const settings = join(dir, 'conf', 'settings.json');
    writeFileSync(settings, JSON.stringify(SETTINGS));
    // a model configuration with a declared key, for the server page's Models section
    const models = join(dir, 'conf', 'models.json');
    writeFileSync(models, JSON.stringify(MODELS));
    const key = join(dir, 'conf', 'anthropic.key');
    writeFileSync(key, DECLARED_KEY, { mode: 0o600 });
    const openServer = await serve(
      bin,
      join(dir, 'open-data'),
      join(dir, 'open-server.log'),
      [
        '--backup-config',
        backupConfig,
        '--settings',
        settings,
        '--mem',
        DECLARED_DATASET,
        '--model-config',
        models,
        '--model-secret',
        `anthropic=file:${key}`,
      ],
      1,
    );
    open = openServer.child;
    process.env.SPARKLES_E2E_OPEN_URL = openServer.url;
    process.env.SPARKLES_E2E_REPOS = repos;
  } catch (e) {
    process.off('exit', kill);
    await stop(child);
    if (open) await stop(open);
    throw new Error(`${e instanceof Error ? e.message : e} (server log: ${log})`, { cause: e });
  }

  return async () => {
    process.off('exit', kill);
    await stop(child);
    if (open) await stop(open);
    if (process.env.SPARKLES_E2E_KEEP) console.log(`sparkles e2e: kept ${dir}`);
    else rmSync(dir, { recursive: true, force: true });
  };
}
