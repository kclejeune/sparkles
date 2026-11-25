// Start a Sparkles test server and discover the address it bound.

import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { appendFileSync, closeSync, openSync, readFileSync } from 'node:fs';
import { Agent, get } from 'node:http';
import { setTimeout as delay } from 'node:timers/promises';

const direct = new Agent();
const record =
  /^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z +INFO sparkles: Sparkles \S+ listening on (http:\/\/127\.0\.0\.1:([1-9][0-9]{0,4}))\/ \(UI at \/ui\/\), opening [0-9]+ datasets\r?\n/gm;

/** @param {string} output */
function listenerUrl(output) {
  for (const match of output.matchAll(record)) {
    if (Number(match[2]) <= 65535) return match[1];
  }
}

/** @param {import('node:child_process').ChildProcess} child */
function assertRunning(child) {
  if (child.exitCode !== null || child.signalCode !== null)
    throw new Error(`exited with ${child.signalCode ?? child.exitCode}`);
}

/**
 * @param {import('node:child_process').ChildProcess} child
 * @param {number} graceMs
 */
export async function stopServer(child, graceMs = 10_000) {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise((resolve) => child.once('exit', resolve));
  child.kill('SIGTERM');
  const timer = setTimeout(() => child.kill('SIGKILL'), graceMs);
  try {
    await exited;
  } finally {
    clearTimeout(timer);
  }
}

/**
 * @param {import('node:child_process').ChildProcess} child
 * @param {string} url
 * @param {number} timeout
 * @param {AbortSignal} [signal]
 */
export async function waitForReady(child, url, timeout, signal) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    signal?.throwIfAborted();
    assertRunning(child);
    const ready = await new Promise((resolve) => {
      const request = get(url, { agent: direct, signal: AbortSignal.timeout(1000) }, (response) => {
        response.resume();
        resolve(response.statusCode === 200);
      });
      request.once('error', () => resolve(false));
    });
    signal?.throwIfAborted();
    assertRunning(child);
    if (ready) return;
    await delay(100);
  }
  throw new Error(`${url} did not become ready within ${timeout / 1000} s`);
}

/** @param {string} log */
export function serverLogTail(log) {
  return readFileSync(log, 'utf8').split('\n').slice(-30).join('\n');
}

/**
 * @param {string} bin
 * @param {string[]} args
 * @param {string} log
 * @param {{ timeout?: number, readyPath?: string, echo?: boolean, signal?: AbortSignal }} options
 */
export async function startServer(bin, args, log, options = {}) {
  const { timeout = 30_000, readyPath = '/$/ready', echo = false, signal } = options;
  const out = openSync(log, 'w');
  const child = spawn(bin, args, {
    stdio: ['ignore', echo ? 'inherit' : out, echo ? 'pipe' : out],
    env: { ...process.env, RUST_LOG: 'info', NO_COLOR: '1' },
  });
  closeSync(out);
  if (echo) {
    child.stderr?.on('data', (data) => {
      appendFileSync(log, data);
      process.stderr.write(data);
    });
  }
  /** @type {Error | undefined} */
  let spawnError;
  child.once('error', (error) => (spawnError = error));
  const kill = () => child.kill('SIGKILL');
  process.once('exit', kill);
  try {
    const deadline = Date.now() + timeout;
    while (Date.now() < deadline) {
      signal?.throwIfAborted();
      if (spawnError) throw spawnError;
      assertRunning(child);
      const url = listenerUrl(readFileSync(log, 'utf8'));
      if (url) {
        await waitForReady(child, url + readyPath, deadline - Date.now(), signal);
        return { child, url };
      }
      await delay(50);
    }
    throw new Error(`did not report its listener within ${timeout / 1000} s`);
  } catch (error) {
    await stopServer(child);
    throw new Error(
      `sparkles serve ${error instanceof Error ? error.message : error}. Log ${log}:\n${serverLogTail(log)}`,
      { cause: error },
    );
  } finally {
    process.off('exit', kill);
  }
}

/**
 * @param {string} bin
 * @param {string[]} args
 * @param {string} log
 * @param {number} timeout
 * @param {(child: import('node:child_process').ChildProcess, url: string) => Promise<void>} setup
 */
export async function runSeedServer(bin, args, log, timeout, setup) {
  const controller = new AbortController();
  /** @type {import('node:child_process').ChildProcess | undefined} */
  let child;
  const stop = () => {
    controller.abort();
    if (child) void stopServer(child);
  };
  process.once('SIGINT', stop);
  process.once('SIGTERM', stop);
  try {
    const server = await startServer(bin, args, log, {
      timeout,
      readyPath: '/$/ping',
      echo: true,
      signal: controller.signal,
    });
    child = server.child;
    const exited = once(child, 'exit');
    await setup(child, server.url);
    if (!controller.signal.aborted) console.log(`ready ${server.url}`);
    const [code, signal] = await exited;
    if (!controller.signal.aborted) throw new Error(`sparkles serve exited with ${signal ?? code}`);
  } catch (error) {
    if (!controller.signal.aborted) throw error;
  } finally {
    process.off('SIGINT', stop);
    process.off('SIGTERM', stop);
    if (child) await stopServer(child);
  }
}
