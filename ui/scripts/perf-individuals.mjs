// Compare the capped Individuals opening with paged type/member reads. This uses one
// backend for both UI builds, warms the shared ontology mapping before browser trials,
// and records the cold root discovery and warm member scans separately.
import { chromium, expect } from '@playwright/test';
import { createServer, request as proxyRequest } from 'node:http';
import {
  createReadStream,
  existsSync,
  statSync,
  writeFileSync,
  readFileSync,
  readdirSync,
} from 'node:fs';
import { resolve, extname } from 'node:path';
import { createHash } from 'node:crypto';
import os from 'node:os';
import { captureFiniteReads } from './finite-reads.mjs';

const backend = process.env.INDIVIDUALS_BACKEND;
const before = process.env.INDIVIDUALS_BEFORE_UI;
const after = process.env.INDIVIDUALS_AFTER_UI ?? resolve('build');
const output = process.env.INDIVIDUALS_OUT;
if (!backend || !before || !output)
  throw new Error('Set INDIVIDUALS_BACKEND, INDIVIDUALS_BEFORE_UI and INDIVIDUALS_OUT.');
if (existsSync(output))
  throw new Error('The output already exists. Keep raw run records immutable.');
if (
  ![process.env.INDIVIDUALS_BEFORE_SOURCE, process.env.INDIVIDUALS_AFTER_SOURCE].every(
    (source) => typeof source === 'string' && /^[0-9a-f]{40}$/.test(source),
  )
)
  throw new Error('Set both INDIVIDUALS source identities to full Git commit IDs.');
const count = Number(process.env.INDIVIDUALS_COUNT ?? 50250);
const repeats = Number(process.env.INDIVIDUALS_REPEATS ?? 3);
if (!Number.isInteger(count) || count < 200 || !Number.isInteger(repeats) || repeats < 1)
  throw new Error('Use at least 200 individuals and one repetition.');
const ds = 'individuals-perf607';
const ns = 'http://example.org/individuals607/';
const individual = (n) => ns + 'i' + String(n).padStart(6, '0');
const parts = [
  '@prefix owl: <http://www.w3.org/2002/07/owl#> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .',
  `<${ns}People> a owl:Class ; rdfs:label "People"@en . <${ns}Other> a owl:Class ; rdfs:label "Other type"@en .`,
  `<${ns}untyped> a owl:NamedIndividual . <${ns}knows> a owl:ObjectProperty . <${ns}subjectOnly> <${ns}knows> <${ns}objectOnly> .`,
];
for (let n = 0; n < count; n++)
  parts.push(
    `<${individual(n)}> a owl:NamedIndividual, <${ns}People> ; rdfs:label "individual ${String(n).padStart(6, '0')}"@en .`,
  );
parts.push(`<${individual(count - 1)}> a <${ns}Other> . [] a <${ns}People> .`);
const turtle = parts.join('\n');
function buildDigest(dir) {
  const hash = createHash('sha256');
  function walk(path, relative = '') {
    for (const entry of readdirSync(path, { withFileTypes: true }).sort((a, b) =>
      a.name < b.name ? -1 : a.name > b.name ? 1 : 0,
    )) {
      const child = resolve(path, entry.name);
      const name = relative + entry.name;
      if (entry.isDirectory()) walk(child, name + '/');
      else if (entry.isFile()) hash.update(name + '\0').update(readFileSync(child));
    }
  }
  walk(resolve(dir));
  return hash.digest('hex');
}
const result = {
  runAt: new Date().toISOString(),
  node: process.version,
  harnessSha256: createHash('sha256')
    .update(readFileSync(import.meta.filename))
    .digest('hex'),
  accountingSha256: createHash('sha256')
    .update(readFileSync(new URL('./finite-reads.mjs', import.meta.url)))
    .digest('hex'),
  uiBuilds: { before: buildDigest(before), after: buildDigest(after) },
  browserHost: { hostname: os.hostname(), cpu: os.cpus()[0].model, platform: os.platform() },
  beforeSource: process.env.INDIVIDUALS_BEFORE_SOURCE,
  afterSource: process.env.INDIVIDUALS_AFTER_SOURCE,
  serverReceipt: process.env.INDIVIDUALS_SERVER_RECEIPT
    ? JSON.parse(readFileSync(process.env.INDIVIDUALS_SERVER_RECEIPT, 'utf8'))
    : null,
  fixture: {
    count,
    bytes: Buffer.byteLength(turtle),
    sha256: createHash('sha256').update(turtle).digest('hex'),
  },
  mode: 'Fresh browser contexts and production UI builds, shared warm server mapping; cold root discovery recorded separately. Long-lived change feeds excluded from transfer accounting.',
  api: [],
  browser: [],
};
async function api(path, init) {
  const start = performance.now();
  const response = await fetch(backend + path, init);
  const text = await response.text();
  if (!response.ok) throw new Error(`${response.status} ${path}: ${text}`);
  result.api.push({
    path,
    ms: performance.now() - start,
    requestBodyBytes: init?.body ? Buffer.byteLength(init.body) : 0,
    responseBytes: Buffer.byteLength(text),
    commit: response.headers.get('sparkles-commit'),
  });
  return text ? JSON.parse(text) : null;
}
const made = await fetch(backend + '/$/datasets', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ dbName: ds, dbType: 'persistent' }),
});
if (!made.ok && made.status !== 409) throw new Error(await made.text());
const loaded = await fetch(backend + '/' + ds + '/data', {
  method: 'PUT',
  headers: { 'Content-Type': 'text/turtle' },
  body: turtle,
});
if (!loaded.ok) throw new Error(await loaded.text());
const route = '/$/ontology/' + ds + '/individuals';
const root = await api(route + '?limit=100');
expect(root.individuals).toHaveLength(0);
expect(root.groups.map((g) => [g.type?.iri ?? null, g.total])).toEqual([
  [null, 3],
  [ns + 'Other', 1],
  [ns + 'People', count],
]);
const common = new URLSearchParams({
  type: ns + 'People',
  at: 'commit:' + root.commit,
  limit: '100',
});
const first = await api(route + '?' + common);
expect(first.individuals).toHaveLength(100);
expect(first.total).toBe(count);
common.set('after', first.next);
const second = await api(route + '?' + common);
expect(new Set([...first.individuals, ...second.individuals].map((n) => n.iri)).size).toBe(200);
const located = await api(
  route + '?' + new URLSearchParams({ iri: individual(count - 1), at: 'commit:' + root.commit }),
);
expect(located.individuals[0].types).toHaveLength(2);
const object = await api(
  '/$/ontology/' +
    ds +
    '/entity?' +
    new URLSearchParams({ iri: ns + 'objectOnly', at: 'commit:' + root.commit }),
);
expect(object.kinds).toContain('NamedIndividual');

const mime = {
  '.html': 'text/html',
  '.js': 'text/javascript',
  '.css': 'text/css',
  '.json': 'application/json',
  '.woff2': 'font/woff2',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
};
async function serve(dir, port) {
  const rootDir = resolve(dir);
  const server = createServer((req, res) => {
    const url = new URL(req.url, 'http://localhost');
    if (!url.pathname.startsWith('/ui')) {
      const upstream = proxyRequest(
        new URL(req.url, backend),
        { method: req.method, headers: req.headers },
        (response) => {
          res.writeHead(response.statusCode, response.headers);
          response.pipe(res);
        },
      );
      res.on('close', () => {
        if (!res.writableEnded) upstream.destroy();
      });
      upstream.on('error', (e) => {
        if (res.destroyed) return;
        if (!res.headersSent) res.writeHead(502);
        res.end(String(e));
      });
      req.pipe(upstream);
      return;
    }
    let path = resolve(rootDir, '.' + decodeURIComponent(url.pathname.slice(3)));
    if (!path.startsWith(rootDir + '/') || !existsSync(path) || !statSync(path).isFile())
      path = resolve(rootDir, 'index.html');
    res.setHeader('Content-Type', mime[extname(path)] ?? 'application/octet-stream');
    if (req.headers['accept-encoding']?.includes('br') && existsSync(path + '.br')) {
      path += '.br';
      res.setHeader('Content-Encoding', 'br');
    }
    res.setHeader('Content-Length', statSync(path).size);
    createReadStream(path).pipe(res);
  });
  await new Promise((ok) => server.listen(port, '127.0.0.1', ok));
  return server;
}
const servers = [await serve(before, 45230), await serve(after, 45231)];
const browser = await chromium.launch({ channel: 'chromium' });
result.browserVersion = browser.version();
try {
  for (let rep = 0; rep < repeats; rep++) {
    for (const which of rep % 2 ? ['after', 'before'] : ['before', 'after']) {
      const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
      const page = await context.newPage();
      const capture = captureFiniteReads(page, ds);
      const { requests, pending } = capture;
      const start = performance.now();
      await page.goto(
        `http://127.0.0.1:${which === 'before' ? 45230 : 45231}/ui/workbench/ontology?ds=${ds}&kind=NamedIndividual`,
      );
      const tree = page.getByRole('tree', { name: 'Individuals (asserted)' });
      let firstBucketsMs = null;
      if (which === 'after') {
        await expect(tree.getByRole('button', { name: 'People', exact: true })).toBeVisible({
          timeout: 120000,
        });
        firstBucketsMs = performance.now() - start;
        await tree.getByRole('button', { name: 'Expand People', exact: true }).click();
      }
      await expect(
        tree.getByRole('button', { name: 'individual 000000', exact: true }),
      ).toBeVisible({ timeout: 120000 });
      const firstMemberMs = performance.now() - start;
      await expect.poll(() => pending.size, { timeout: 120000 }).toBe(0);
      await capture.settle();
      const rows = await tree.getByRole('treeitem').count();
      expect(rows).toBeLessThan(80);
      if (which === 'after') {
        expect(
          requests.filter((r) => r.path.endsWith('/entities') || r.path.endsWith('/axioms')),
        ).toHaveLength(0);
        expect(requests.filter((r) => r.path.endsWith('/individuals'))).toHaveLength(2);
      } else {
        expect(
          requests.some((r) => r.path.endsWith('/entities') && r.query.includes('NamedIndividual')),
        ).toBe(true);
        expect(
          requests.some((r) => r.path.endsWith('/axioms') && r.query.includes('ClassAssertion')),
        ).toBe(true);
        expect(requests.filter((r) => r.path.endsWith('/individuals'))).toHaveLength(0);
      }
      await context.close();
      await capture.settle();
      result.browser.push({
        which,
        rep,
        firstBucketsMs,
        firstMemberMs,
        rows,
        requests,
        ...capture.summary(),
      });
    }
  }
  result.correctness = `${count} typed named members, 3 untyped members, duplicate-free continuation, two memberships on the final individual, anonymous omission, incoming-only frame lookup and virtualized actual-tab display checked.`;
  result.complete = result.browser.every((trial) => trial.complete);
  writeFileSync(output, JSON.stringify(result, null, 2) + '\n', { flag: 'wx' });
  if (!result.complete)
    throw new Error('Incomplete finite-read accounting. Raw trials preserved at ' + output);
  console.log(
    JSON.stringify(
      result.browser.map(
        ({
          which,
          rep,
          firstBucketsMs,
          firstMemberMs,
          decodedBytes,
          requestBytes,
          responseWireBytes,
        }) => ({
          which,
          rep,
          firstBucketsMs,
          firstMemberMs,
          decodedBytes,
          requestBytes,
          responseWireBytes,
        }),
      ),
      null,
      2,
    ),
  );
} finally {
  await browser.close();
  await Promise.all(servers.map((s) => new Promise((ok) => s.close(ok))));
}
