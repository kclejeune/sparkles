import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import {
  runSeedServer,
  startServer,
  stopServer,
  waitForReady,
} from '../../../ui/scripts/server-process.mjs';

test('the startup probe uses a complete Sparkles record', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'sparkles-startup-parser-'));
  const requests = [];
  const server = createServer((request, response) => {
    requests.push(request.url);
    response.end();
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const { port } = server.address();
  let child;
  try {
    const output = [
      '2026-01-01T00:00:00Z  INFO wrapper: Sparkles test listening on http://127.0.0.1:1/ (UI at /ui/), opening 0 datasets',
      '2026-01-01T00:00:00Z  INFO sparkles: Sparkles test listening on http://127.0.0.1:65536/ (UI at /ui/), opening 0 datasets',
      `2026-01-01T00:00:00Z  INFO sparkles: Sparkles test listening on http://127.0.0.1:${port}/ (UI at /ui/), opening 0 datasets`,
    ].join('\n');
    const running = await startServer(
      process.execPath,
      ['-e', `console.error(${JSON.stringify(output)}); setInterval(() => {}, 1000);`],
      join(dir, 'server.log'),
      { timeout: 2000 },
    );
    child = running.child;
    assert.equal(running.url, `http://127.0.0.1:${port}`);
    assert.deepEqual(requests, ['/$/ready']);
  } finally {
    if (child) await stopServer(child);
    server.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test('successful readiness received after the child exits is rejected', async () => {
  const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000);']);
  const server = createServer(async (_request, response) => {
    await stopServer(child);
    response.end();
  });
  try {
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    await assert.rejects(
      waitForReady(child, `http://127.0.0.1:${server.address().port}/$/ready`, 2000),
      /exited with/,
    );
  } finally {
    await stopServer(child);
    server.close();
  }
});

test('child shutdown escalates and reaps a process that ignores SIGTERM', async () => {
  const child = spawn(process.execPath, [
    '-e',
    "process.on('SIGTERM', () => {}); console.log('started'); setInterval(() => {}, 1000);",
  ]);
  try {
    await once(child.stdout, 'data');
    await stopServer(child, 30);
    assert.equal(child.signalCode, 'SIGKILL');
  } finally {
    if (child.signalCode === null) await stopServer(child, 30);
  }
});

// A descendant keeps the pipe open briefly after the child process exits.
const lateStderr = `require('node:child_process')
  .spawn(process.execPath, ['-e', "setTimeout(() => console.error('final diagnostic'), 250)"],
    { stdio: ['ignore', 'ignore', 'inherit'] }).unref();`;

test('failed startup includes stderr received after child exit', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'sparkles-startup-stderr-'));
  try {
    const helper = new URL('../../../ui/scripts/server-process.mjs', import.meta.url);
    const childCode = `${lateStderr} process.exit(23);`;
    const output = execFileSync(
      process.execPath,
      [
        '--input-type=module',
        '-e',
        `
      import { startServer } from ${JSON.stringify(helper.href)};
      try {
        await startServer(process.execPath, ['-e', ${JSON.stringify(childCode)}],
          ${JSON.stringify(join(dir, 'server.log'))}, { echo: true, timeout: 2000 });
      } catch (error) {
        console.log(error.message);
      }
    `,
      ],
      { encoding: 'utf8', timeout: 5000, stdio: ['ignore', 'pipe', 'ignore'] },
    );
    assert.match(output, /exited with 23[\s\S]*final diagnostic/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('shutdown bounds the drain of stderr inherited by a descendant', async () => {
  const child = spawn(process.execPath, ['-e', `${lateStderr} process.exit(23);`]);
  let stderr = '';
  child.stderr.on('data', (data) => (stderr += data));
  const closed = once(child, 'close');
  try {
    await once(child, 'exit');
    await stopServer(child, 30);
    assert(child.stderr.closed);
    assert.equal(stderr, '');
  } finally {
    await closed;
  }
});

test('seed shutdown drains echoed stderr and keeps only startup output in its log', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'sparkles-seed-stderr-'));
  const log = join(dir, 'server.log');
  const server = createServer((_request, response) => response.end());
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const url = `http://127.0.0.1:${server.address().port}`;
  const record = `2026-01-01T00:00:00Z  INFO sparkles: Sparkles test listening on ${url}/ (UI at /ui/), opening 0 datasets`;
  let child;
  let closed;
  let echoed = '';
  try {
    const running = await startServer(
      process.execPath,
      [
        '-e',
        `process.on('SIGTERM', () => { ${lateStderr} process.exit(23); });
        console.error(${JSON.stringify(record)}); setInterval(() => {}, 1000);`,
      ],
      log,
      { echo: true, timeout: 2000 },
    );
    child = running.child;
    closed = once(child, 'close');
    child.stderr.on('data', (data) => (echoed += data));
    await stopServer(child);
    assert.match(echoed, /final diagnostic/);
    assert.equal(await readFile(log, 'utf8'), record + '\n');
  } finally {
    if (child) await stopServer(child);
    await closed;
    server.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test('seed setup finishing after child exit does not announce readiness', async (t) => {
  const dir = await mkdtemp(join(tmpdir(), 'sparkles-seed-exit-'));
  const server = createServer((_request, response) => response.end());
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const url = `http://127.0.0.1:${server.address().port}`;
  const record = `2026-01-01T00:00:00Z  INFO sparkles: Sparkles test listening on ${url}/ (UI at /ui/), opening 0 datasets`;
  const output = t.mock.method(console, 'log', () => {});
  let setupCompleted = false;
  try {
    await assert.rejects(
      runSeedServer(
        process.execPath,
        ['-e', `console.error(${JSON.stringify(record)}); setInterval(() => {}, 1000);`],
        join(dir, 'server.log'),
        2000,
        async (child) => {
          const exited = once(child, 'exit');
          child.kill('SIGTERM');
          await exited;
          setupCompleted = true;
        },
      ),
      /exited with SIGTERM/,
    );
    assert(setupCompleted);
    assert.equal(output.mock.callCount(), 0);
  } finally {
    server.close();
    await rm(dir, { recursive: true, force: true });
  }
});

test(
  'design seed sends readiness and setup writes directly to its child',
  {
    skip: !process.allowedNodeEnvironmentFlags.has('--use-env-proxy'),
    timeout: 10_000,
  },
  async () => {
    const dir = await mkdtemp(join(tmpdir(), 'sparkles-seed-proxy-'));
    const fixture = join(dir, 'sparkles');
    const requests = join(dir, 'requests');
    await writeFile(
      fixture,
      `#!${process.execPath}
    const { appendFileSync } = require('node:fs');
    const { createServer } = require('node:http');
    if (process.argv[2] === 'load') process.exit(0);
    const server = createServer(async (request, response) => {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = Buffer.concat(chunks).toString();
      appendFileSync(process.env.REQUESTS, JSON.stringify([request.method, request.url, body]) + '\\n');
      response.writeHead(request.method === 'GET' ? 200 : 204).end();
    });
    server.listen(0, '127.0.0.1', () => {
      console.error('2026-01-01T00:00:00Z  INFO sparkles: Sparkles test listening on http://127.0.0.1:'
        + server.address().port + '/ (UI at /ui/), opening 1 datasets');
    });
  `,
      { mode: 0o755 },
    );
    const proxyRequests = [];
    const proxy = createServer((request, response) => {
      proxyRequests.push(request.url);
      response.writeHead(502).end();
    });
    proxy.on('connect', (request, socket) => {
      proxyRequests.push(request.url);
      socket.end('HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n');
    });
    proxy.listen(0, '127.0.0.1');
    await once(proxy, 'listening');
    const proxyUrl = `http://127.0.0.1:${proxy.address().port}`;
    const seed = fileURLToPath(new URL('../../../ui/scripts/seed-design.mjs', import.meta.url));
    const child = spawn(process.execPath, [seed], {
      env: {
        ...process.env,
        http_proxy: proxyUrl,
        HTTP_PROXY: proxyUrl,
        no_proxy: 'localhost',
        NO_PROXY: 'localhost',
        NODE_USE_ENV_PROXY: '1',
        REQUESTS: requests,
        SPARKLES_BIN: fixture,
        SPARKLES_SEED_PORT: '0',
        SPARKLES_SEED_SKIP: 'go,unesco',
        SPARKLES_SEED_ASK: '1',
      },
    });
    const closed = once(child, 'close');
    let diagnostics = '';
    child.stderr.on('data', (data) => (diagnostics += data));
    let ready;
    try {
      for await (const line of createInterface({ input: child.stdout })) {
        if (line.startsWith('ready ')) {
          ready = line;
          break;
        }
      }
      assert.deepEqual(proxyRequests, []);
      assert.match(ready ?? diagnostics, /^ready http:\/\/127\.0\.0\.1:\d+$/);
      const rows = (await readFile(requests, 'utf8'))
        .trim()
        .split('\n')
        .map((line) => JSON.parse(line));
      assert.deepEqual(
        rows.map(([method, path]) => [method, path]),
        [
          ['GET', '/$/ping'],
          ['GET', '/$/ready/demo'],
          ['PUT', '/$/queries/demo/people-by-city'],
          ['PUT', '/$/queries/demo/class-sizes'],
          ['PUT', '/$/queries/demo/labels-matching'],
          ['POST', '/$/branches/demo'],
          ['POST', '/$/branches/demo'],
          ['PUT', '/$/assistant/demo'],
        ],
      );
      assert.deepEqual(
        rows.slice(5, 7).map(([, , body]) => JSON.parse(body).name),
        ['draft-places', 'review-2026'],
      );
      assert.equal(JSON.parse(rows[7][2]).enabled, true);
    } finally {
      await stopServer(child);
      await closed;
      proxy.close();
      await rm(dir, { recursive: true, force: true });
    }
  },
);
