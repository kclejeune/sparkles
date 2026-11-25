import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { startServer, stopServer, waitForReady } from '../../../ui/scripts/server-process.mjs';

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
