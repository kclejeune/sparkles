import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { SparklesClient, factory, BudgetExceededError, ConflictError } from '../dist/index.js';

const binary = process.env.SPARKLES_CLIENT_TEST_SERVER_BIN;
async function freePort() {
  const socket = createServer();
  await new Promise((resolve, reject) =>
    socket.listen(0, '127.0.0.1', resolve).once('error', reject),
  );
  const port = socket.address().port;
  await new Promise((resolve, reject) =>
    socket.close((error) => (error ? reject(error) : resolve())),
  );
  return port;
}

test(
  'real Sparkles server: memory branch isolation, streaming queries, snapshots, GSP ETags, history and typed administration',
  { skip: !binary, timeout: 30_000 },
  async () => {
    const directory = await mkdtemp(join(tmpdir(), 'sparkles-client-server-'));
    let server;
    let output = '';
    try {
      const port = await freePort();
      server = spawn(binary, [
        'serve',
        '--data',
        directory,
        '--host',
        '127.0.0.1',
        '--port',
        String(port),
        '--mem',
        'wiki',
        '--cache-mb',
        '16',
        '--result-cache-mb',
        '0',
        '--history-cache-mb',
        '16',
        '--service-cache-mb',
        '0',
      ]);
      server.stderr.on('data', (data) => {
        output += data;
      });
      server.stdout.on('data', (data) => {
        output += data;
      });
      let startupError;
      server.once('error', (error) => {
        startupError = error;
      });
      const base = `http://127.0.0.1:${port}`;
      const startupDeadline = Date.now() + 15_000;
      for (;;) {
        try {
          if ((await fetch(base + '/$/datasets')).ok) break;
        } catch {
          /* Await startup. */
        }
        if (startupError) throw startupError;
        if (server.exitCode !== null || server.signalCode !== null || Date.now() >= startupDeadline)
          throw new Error('Server did not start: ' + output);
        await new Promise((resolve) => setTimeout(resolve, 25));
      }
      const client = new SparklesClient(base),
        main = client.dataset('wiki');
      const inserted = await main.update('INSERT DATA { ex:s ex:p "héllo"@fr }', {
        prefixes: { ex: 'urn:' },
        message: 'first',
      });
      assert.equal(inserted.inserted, 1n);
      assert.equal(inserted.receipt.commit.seq, 1n);
      const create = await client.api.POST('/$/branches/{ds}', {
        params: { path: { ds: 'wiki' } },
        body: { name: 'work' },
      });
      assert.equal(create.response.status, 201);
      const branch = client.dataset('wiki', 'work');
      await branch.update('INSERT DATA { <urn:t> <urn:p> 2 }', { message: 'branch-only' });
      assert(await branch.ask('ASK { <urn:t> <urn:p> 2 }'));
      assert.equal(await main.ask('ASK { <urn:t> <urn:p> 2 }'), false);
      const rows = await (
        await branch.select('SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY ?s')
      ).toArray();
      assert.equal(rows.length, 2);
      assert.equal(rows[0].get('o').language, 'fr');
      const old = await branch.ask('ASK { <urn:t> <urn:p> 2 }', { at: 1n });
      assert.equal(old, false);
      const page = await branch.commits({ limit: 1 });
      assert.equal(page.commits[0].message, 'branch-only');
      assert.equal(page.commits[0].seq, 2n);
      assert.equal(page.head, 2n);
      const next = await branch.commits({ limit: 1, before: 2n });
      assert.equal(next.commits[0].seq, 1n);
      const graph = factory.namedNode('urn:g');
      await branch.graphs.put(graph, '<urn:g-s> <urn:p> "graph" .', {
        contentType: 'application/n-triples',
        message: 'graph',
      });
      const response = await branch.graphs.get(graph);
      const etag = response.headers.get('etag');
      assert(etag);
      assert((await response.text()).includes('graph'));
      await branch.graphs.put(graph, '<urn:g-s> <urn:p> "updated" .', {
        contentType: 'application/n-triples',
        ifMatch: etag,
      });
      await assert.rejects(
        branch.graphs.put(graph, '<urn:g-s> <urn:p> "stale" .', {
          contentType: 'application/n-triples',
          ifMatch: etag,
        }),
        ConflictError,
      );
      assert.equal(
        (await (await branch.construct('CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }')).toArray())
          .length,
        2,
      );
      await assert.rejects(
        async () => (await branch.select('SELECT ?s WHERE { ?s ?p ?o }', { maxRows: 1 })).toArray(),
        BudgetExceededError,
      );
      const administrative = await client.api.GET('/$/datasets/{name}', {
        params: { path: { name: 'wiki' } },
      });
      assert.equal(administrative.data.name, 'wiki');
      const renamed = await client.api.PATCH('/$/branches/{ds}/{name}', {
        params: { path: { ds: 'wiki', name: 'work' } },
        body: { name: 'renamed' },
      });
      assert.equal(renamed.response.status, 200);
      assert(await client.dataset('wiki', 'renamed').ask('ASK { <urn:t> <urn:p> 2 }'));
    } finally {
      if (server && server.exitCode === null) {
        const exited = new Promise((resolve) => server.once('exit', resolve));
        server.kill('SIGTERM');
        await exited;
      }
      await rm(directory, { recursive: true, force: true });
    }
  },
);
