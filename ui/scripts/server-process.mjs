// Start a Sparkles test server and discover the address it bound.

import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { closeSync, openSync, readFileSync, writeSync } from 'node:fs';
import { Agent, get, request } from 'node:http';
import { pipeline, Readable } from 'node:stream';
import { setTimeout as delay } from 'node:timers/promises';

const direct = new Agent();
const record =
  /^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:.]+Z +INFO sparkles: Sparkles \S+ listening on (http:\/\/127\.0\.0\.1:([1-9][0-9]{0,4}))\/ \(UI at \/ui\/\), opening [0-9]+ datasets\r?\n/gm;

/**
 * Send a request directly to the local server and return redirects to the caller.
 * @param {Request} input
 * @returns {Promise<Response>}
 */
export async function fetchLocal(input) {
  const url = new URL(input.url);
  if (url.protocol !== 'http:' || url.hostname !== '127.0.0.1') {
    throw new Error(`Expected a local Sparkles URL, got ${url}`);
  }
  return new Promise((resolve, reject) => {
    const outgoing = request(
      url,
      {
        agent: direct,
        method: input.method,
        headers: Object.fromEntries(input.headers),
        signal: input.signal,
      },
      (incoming) => {
        const headers = new Headers();
        for (let i = 0; i < incoming.rawHeaders.length; i += 2) {
          headers.append(incoming.rawHeaders[i], incoming.rawHeaders[i + 1]);
        }
        const status = incoming.statusCode;
        const empty = input.method === 'HEAD' || status === 204 || status === 205 || status === 304;
        if (empty) incoming.resume();
        const body = empty
          ? null
          : Readable.toWeb(incoming, {
              strategy: {
                highWaterMark: incoming.readableHighWaterMark,
                size: (chunk) => chunk.length,
              },
            });
        resolve(
          // Node fetch and node:stream/web use the same Web streams at runtime.
          new Response(/** @type {BodyInit | null} */ (/** @type {unknown} */ (body)), {
            headers,
            status,
            statusText: incoming.statusMessage,
          }),
        );
      },
    );
    outgoing.once('error', reject);
    if (input.body) {
      const body = /** @type {import('node:stream/web').ReadableStream<Uint8Array>} */ (
        /** @type {unknown} */ (input.body)
      );
      pipeline(Readable.fromWeb(body), outgoing, (error) => {
        if (error) reject(error);
      });
    } else {
      outgoing.end();
    }
  });
}

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
  if (child.pid && child.exitCode === null && child.signalCode === null) {
    const exited = new Promise((resolve) => child.once('exit', resolve));
    child.kill('SIGTERM');
    const timer = setTimeout(() => child.kill('SIGKILL'), graceMs);
    try {
      await exited;
    } finally {
      clearTimeout(timer);
    }
  }
  // Drain final diagnostics, with a deadline in case a descendant inherited the pipe.
  const stderr = child.stderr;
  if (stderr && !stderr.closed) {
    const drained = new Promise((resolve) => stderr.once('close', resolve));
    const timer = setTimeout(() => stderr.destroy(), graceMs);
    await drained;
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
  let captureLog = echo;
  if (echo) {
    child.stderr?.on('data', (data) => {
      if (captureLog) writeSync(out, data);
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
    captureLog = false;
    closeSync(out);
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
    await setup(child, server.url);
    if (controller.signal.aborted) return;
    assertRunning(child);
    const exited = once(child, 'exit');
    console.log(`ready ${server.url}`);
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
