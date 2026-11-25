// Starts a real `sparkles serve` for the end-to-end tests: a temporary data directory, a
// kernel-assigned port on 127.0.0.1 and an auth configuration with one local user and one static API
// token. It loads a small dataset (labels, comments for full-text search, vectors for
// "Similar"), signs the user in, and hands the details to the tests through environment
// variables (inherited by the workers). A second server without auth (the default
// `sparkles serve`) starts with only an in-memory dataset the operator declares, a
// settings file that also locks a model provider's endpoint, a model configuration with
// a declared key, and a backup config that lets its API register repositories under a
// temporary directory. It allows private outbound addresses, so that a notification test
// reaches a receiver the test runs on 127.0.0.1. The returned function is the global
// teardown.
//
// SPARKLES_BIN selects the binary (default: ../target/debug/sparkles, which serves ui/build
// from disk); SPARKLES_E2E_KEEP=1 keeps the data directory and server log;
// SPARKLES_E2E_PORT=N runs the servers on ports N and N + 1 instead of free ones;
// SPARKLES_E2E_NO_WASM=1 runs against a debug server whose ui/build lacks the formatter's
// browser module.

import { request, type FullConfig } from '@playwright/test';
import { spawnSync, type ChildProcess } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { BROWSE_TURTLE } from '../browse-fixture';
import { startServer, stopServer } from '../server-process';
import {
  BROWSE_EDIT_AUTH,
  BROWSE_EDIT_OPEN_GRAPH,
  DATASET,
  DISCUSSIONS_AUTH,
  BULK_AUTH,
  IMPORTS_AUTH,
  DATA,
  DECLARED_DATASET,
  DECLARED_KEY,
  MODELS,
  PASSWORD,
  PATHS_DATA,
  PATHS_DATASET,
  PATHS_EX,
  PATHS_PROTECTION,
  PATTERN_DATA,
  PATTERN_DATASET,
  PATTERN_PREDICATE,
  READER,
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

/**
 * The Pattern pane, the plan's pattern and Apply to query in the facets of a query have no
 * server fallback for the formatter's WebAssembly module, so without it in the UI the server
 * reads, their tests fail on a pane that reports "unavailable", far from the cause. The
 * server reads the UI from `SPARKLES_UI_DIR` when that is set, and the debug build reads
 * ui/build from disk. Another binary embeds its own UI or names one in a wrapper, which this
 * check cannot see. It looks for the module's asset only, so a build with
 * `VITE_PATTERN_WASM=off` passes it. `SPARKLES_E2E_NO_WASM=1` skips the check, and the tests
 * that need the module then fail.
 */
function checkModule(bin: string) {
  if (process.env.SPARKLES_E2E_NO_WASM === '1') return;
  // real paths, as the operating system resolves them for the server: Node's join and
  // resolve fold `..` before following a symbolic link such as Boxington's managed target
  const real = (p: string) => {
    try {
      return realpathSync.native(p);
    } catch {
      return null;
    }
  };
  const debug = real(bin) === real(resolve(UI_DIR, '../target/debug/sparkles'));
  const configured = process.env.SPARKLES_UI_DIR || null;
  if (!configured && !debug) return;
  const ui = configured ?? join(UI_DIR, 'build');
  const assets = real(ui) && real(`${ui}/_app/immutable/assets`);
  const built =
    assets !== null && readdirSync(assets).some((f) => /^sparkles_fmt_wasm_bg\..*\.wasm$/.test(f));
  if (!built)
    throw new Error(
      `The UI build in ${ui} has no browser module of the formatter, which the Pattern pane ` +
        'and the facets tests need. Run `mise run ui:e2e`, which builds it with ' +
        '`mise run ui:wasm` before the UI, or set SPARKLES_E2E_NO_WASM=1.',
    );
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function check(r: Response, what: string) {
  if (!r.ok) throw new Error(`${what}: ${r.status} ${await r.text()}`);
  return r;
}

/** `SPARKLES_E2E_PORT`: the first server's port, and the next one the second server's. */
const fixedPort = process.env.SPARKLES_E2E_PORT ? Number(process.env.SPARKLES_E2E_PORT) : null;

/** Start on loopback with a kernel-assigned port unless a port was requested. */
function serve(bin: string, data: string, log: string, extra: string[], offset = 0) {
  const port = fixedPort == null || fixedPort === 0 ? 0 : fixedPort + offset;
  return startServer(
    bin,
    ['serve', '--data', data, '--host', '127.0.0.1', '--port', String(port), ...extra],
    log,
  );
}

export default async function globalSetup(_config: FullConfig) {
  const bin = binary();
  checkModule(bin);
  const dir = mkdtempSync(join(tmpdir(), 'sparkles-e2e-'));
  const log = join(dir, 'server.log');

  // A static token (spk_ + 43 base64url characters), stored in the config as its sha256.
  const token = `spk_${randomBytes(32).toString('base64url')}`;
  // a static token whose grant covers only the default graph of overlay-limited, for
  // the validation overlay's restricted caller (U07 §11.8, A13), of embedding-limited,
  // for the embedding map's (U11 A8), and the hr graph and the inferred graph of
  // why-limited, for the hidden premise of U10 A9
  const limited = `spk_${randomBytes(32).toString('base64url')}`;
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
roles = ["pattern-writer"]

[[users]]
name = "${READER}"
password = "${hashed.stdout.trim()}"
roles = ["pattern-reader"]

[[roles.pattern-writer.grants]]
dataset = "${PATTERN_DATASET}"
level = "write"
lifts = ["work"]

[[roles.pattern-writer.grants]]
dataset = "${PATHS_DATASET}"
level = "read"
lifts = ["${PATHS_PROTECTION}"]

[[roles.pattern-writer.grants]]
dataset = "${BROWSE_EDIT_AUTH}"
level = "read"

[[roles.pattern-writer.grants]]
dataset = "${BROWSE_EDIT_AUTH}"
level = "write"
graphs = ["${BROWSE_EDIT_OPEN_GRAPH}"]

[[roles.pattern-writer.grants]]
dataset = "${DISCUSSIONS_AUTH}"
level = "write"

[[roles.pattern-writer.grants]]
dataset = "${BULK_AUTH}"
level = "write"

[[roles.pattern-writer.grants]]
dataset = "${IMPORTS_AUTH}"
level = "write"

[[roles.pattern-reader.grants]]
dataset = "${PATTERN_DATASET}"
level = "read"

[[roles.pattern-reader.grants]]
dataset = "${DISCUSSIONS_AUTH}"
level = "read"

[[roles.pattern-reader.grants]]
dataset = "${BULK_AUTH}"
level = "read"

[[roles.pattern-reader.grants]]
dataset = "${IMPORTS_AUTH}"
level = "read"

[[roles.pattern-reader.grants]]
dataset = "${BROWSE_EDIT_AUTH}"
level = "read"

[[roles.pattern-reader.grants]]
dataset = "${PATHS_DATASET}"
level = "read"

[[protections]]
name = "${PATHS_PROTECTION}"
dataset = "${PATHS_DATASET}"
predicates = ["${PATHS_EX}worksFor"]
classes = ["${PATHS_EX}Founder"]

[[protections]]
name = "work"
dataset = "${PATTERN_DATASET}"
predicates = ["${PATTERN_PREDICATE}"]

[[tokens]]
name = "e2e-setup"
hash = "sha256:${createHash('sha256').update(token).digest('hex')}"
server = ["server-admin"]

[[tokens]]
name = "e2e-limited"
hash = "sha256:${createHash('sha256').update(limited).digest('hex')}"

[[tokens.grants]]
dataset = "overlay-limited"
level = "read"
graphs = ["urn:x-arq:DefaultGraph"]

[[tokens.grants]]
dataset = "why-limited"
level = "read"
graphs = ["http://example.org/hr", "urn:x-sparkles:inferred"]

[[tokens.grants]]
dataset = "embedding-limited"
level = "read"
graphs = ["urn:x-arq:DefaultGraph"]
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
    // the Pattern pane's dataset, whose ex:worksFor the reader does not see
    await check(
      await fetch(`${url}/$/datasets`, {
        method: 'POST',
        headers: { ...auth, 'Content-Type': 'application/json' },
        body: JSON.stringify({ dbName: PATTERN_DATASET, dbType: 'mem' }),
      }),
      'creating the Pattern dataset',
    );
    await check(
      await fetch(`${url}/${PATTERN_DATASET}/data`, {
        method: 'POST',
        headers: { ...auth, 'Content-Type': 'text/turtle' },
        body: PATTERN_DATA,
      }),
      'loading the Pattern dataset',
    );
    // the ontology of the browser's edit mode under auth (item 630)
    await check(
      await fetch(`${url}/$/datasets`, {
        method: 'POST',
        headers: { ...auth, 'Content-Type': 'application/json' },
        body: JSON.stringify({ dbName: BROWSE_EDIT_AUTH, dbType: 'mem' }),
      }),
      'creating the edit mode dataset',
    );
    await check(
      await fetch(`${url}/${BROWSE_EDIT_AUTH}/data`, {
        method: 'POST',
        headers: { ...auth, 'Content-Type': 'text/turtle' },
        body: BROWSE_TURTLE,
      }),
      'loading the edit mode dataset',
    );
    // the path finder's dataset, in two commits
    await check(
      await fetch(`${url}/$/datasets`, {
        method: 'POST',
        headers: { ...auth, 'Content-Type': 'application/json' },
        body: JSON.stringify({ dbName: PATHS_DATASET, dbType: 'persistent' }),
      }),
      'creating the path finder dataset',
    );
    for (const [i, part] of PATHS_DATA.entries())
      await check(
        await fetch(`${url}/${PATHS_DATASET}/data`, {
          method: 'POST',
          headers: { ...auth, 'Content-Type': 'text/turtle' },
          body: part,
        }),
        `loading part ${i + 1} of the path finder dataset`,
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
    process.env.SPARKLES_E2E_LIMITED_TOKEN = limited;
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
        '--outbound-allow-private',
      ],
      1,
    );
    open = openServer.child;
    process.env.SPARKLES_E2E_OPEN_URL = openServer.url;
    process.env.SPARKLES_E2E_REPOS = repos;
  } catch (e) {
    process.off('exit', kill);
    await stopServer(child);
    if (open) await stopServer(open);
    throw new Error(`${e instanceof Error ? e.message : e} (server log: ${log})`, { cause: e });
  }

  return async () => {
    process.off('exit', kill);
    await stopServer(child);
    if (open) await stopServer(open);
    if (process.env.SPARKLES_E2E_KEEP) console.log(`sparkles e2e: kept ${dir}`);
    else rmSync(dir, { recursive: true, force: true });
  };
}
