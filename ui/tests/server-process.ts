import { spawn, type ChildProcess } from 'node:child_process';
import { closeSync, openSync, readFileSync } from 'node:fs';

/** SIGTERM (a graceful drain), then SIGKILL if the child has not exited. */
export async function stopServer(child: ChildProcess) {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise<void>((resolve) => child.once('exit', () => resolve()));
  child.kill('SIGTERM');
  const timer = setTimeout(() => child.kill('SIGKILL'), 10_000);
  await exited;
  clearTimeout(timer);
}

/** Read the address from this child's log before probing the listener. */
export async function startServer(bin: string, args: string[], log: string) {
  const out = openSync(log, 'w');
  const child = spawn(bin, args, {
    stdio: ['ignore', out, out],
    env: { ...process.env, RUST_LOG: 'info' },
  });
  closeSync(out);
  let spawnError: Error | undefined;
  child.once('error', (error) => (spawnError = error));
  const kill = () => child.kill('SIGKILL');
  process.once('exit', kill);
  try {
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline) {
      if (spawnError) throw spawnError;
      if (child.exitCode !== null || child.signalCode !== null)
        throw new Error(`exited with ${child.signalCode ?? child.exitCode}`);
      const url = readFileSync(log, 'utf8').match(
        / listening on (http:\/\/127\.0\.0\.1:[1-9][0-9]*)\/ \(UI at \/ui\/\)/,
      )?.[1];
      if (url) {
        try {
          const response = await fetch(`${url}/$/ready`, { signal: AbortSignal.timeout(1000) });
          if (response.ok) return { child, url };
        } catch {
          // The child has bound the socket and is starting its HTTP service.
        }
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    throw new Error('did not answer /$/ready within 30 s');
  } catch (error) {
    await stopServer(child);
    const tail = readFileSync(log, 'utf8').split('\n').slice(-30).join('\n');
    throw new Error(
      `sparkles serve ${error instanceof Error ? error.message : error}; log ${log}:\n${tail}`,
      {
        cause: error,
      },
    );
  } finally {
    process.off('exit', kill);
  }
}
