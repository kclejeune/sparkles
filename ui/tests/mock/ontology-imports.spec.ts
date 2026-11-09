// Import resolution in the ontology editor (ROADMAP item 624) against the mock's `uni`
// dataset: the Imports tab draws the closure with a cycle and an unresolved import, adds and
// removes a map entry, loads an XML catalog with its file and fetches an import into a graph;
// the switch that excludes imported axioms; a reader and a protected branch, which see the
// closure and the map without the actions; and the phone layout. Each test works on its own
// branch. The mock shares the map across branches, so each test uses its own import IRIs.

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { expectNoSidewaysScroll } from '../phone';

test.describe.configure({ timeout: 90_000 });

const OWL = 'http://www.w3.org/2002/07/owl#';

async function newBranch(request: APIRequestContext, name: string) {
  const made = await request.post('/$/branches/uni', { data: { name } });
  expect(made.status()).toBe(201);
}

async function update(request: APIRequestContext, branch: string, sparql: string) {
  const r = await request.post(`/uni/update?branch=${branch}`, {
    headers: { 'Content-Type': 'application/sparql-update' },
    data: sparql,
  });
  expect(r.ok(), await r.text()).toBe(true);
}

/** The default graph imports `lib` (which imports `lib2`, which imports `lib` back) and `missing`. */
async function seed(request: APIRequestContext, branch: string, tag: string) {
  await update(
    request,
    branch,
    `INSERT DATA {
      <http://example.org/uni> <${OWL}imports> <http://example.org/${tag}/lib>, <http://example.org/${tag}/missing> .
      GRAPH <http://example.org/${tag}/lib> { <http://example.org/${tag}/lib> <${OWL}imports> <http://example.org/${tag}/lib2> }
      GRAPH <http://example.org/${tag}/lib2> { <http://example.org/${tag}/lib2> <${OWL}imports> <http://example.org/${tag}/lib> }
    }`,
  );
}

async function openImports(page: Page, branch: string, extra: Record<string, string> = {}) {
  await page.goto(
    `/ui/workbench/ontology?${new URLSearchParams({ ds: 'uni', branch, tab: 'imports', ...extra })}`,
  );
  await expect(page.getByRole('region', { name: 'Imports' })).toBeVisible({ timeout: 30_000 });
}

const closure = (page: Page) => page.getByLabel('Imports closure');
const unresolved = (page: Page) => page.getByRole('list', { name: 'Unresolved imports' });
const mapTable = (page: Page) => page.getByRole('table', { name: 'Import map' });

const whoami = (name: string, level: string) => ({
  authEnabled: true,
  principal: { kind: 'token', name },
  method: 'bearer',
  server: [],
  datasets: { uni: level },
  canMintTokens: false,
  logout: false,
});

test('the closure shows cycles and unresolved imports, and the map resolves an import', async ({
  page,
  request,
}) => {
  const branch = 'imports-map';
  await newBranch(request, branch);
  try {
    await seed(request, branch, 'm');
    await openImports(page, branch);
    await expect(page.getByRole('status').filter({ hasText: 'unresolved' })).toContainText(
      '3 imports, 1 unresolved, 2 edges close cycles.',
    );
    await expect(closure(page).getByText('cycle')).toHaveCount(2);
    await expect(unresolved(page)).toContainText('http://example.org/m/missing');
    // a manual entry sends the missing import to a graph of the dataset
    await page.getByRole('textbox', { name: 'Import IRI' }).fill('http://example.org/m/missing');
    await page.getByRole('textbox', { name: 'Graph' }).fill('http://example.org/m/lib2');
    await page.getByRole('button', { name: 'Add entry' }).click();
    await expect(mapTable(page)).toContainText('manual');
    await expect(unresolved(page)).toHaveCount(0);
    await expect(closure(page)).toContainText('by the import map');
    await mapTable(page)
      .getByRole('button', { name: 'Remove the entry for http://example.org/m/missing' })
      .click();
    await expect(unresolved(page)).toContainText('http://example.org/m/missing');
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a catalog loads its file and a fetch loads an import, each one commit', async ({
  page,
  request,
}) => {
  const branch = 'imports-load';
  await newBranch(request, branch);
  try {
    await update(
      request,
      branch,
      `INSERT DATA { <http://example.org/uni> <${OWL}imports> <http://example.org/c/lib>, <http://imports.mock/units.ttl> }`,
    );
    await openImports(page, branch);
    await expect(unresolved(page).locator('li')).toHaveCount(2);
    const catalog = `<?xml version="1.0"?>
<catalog xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog">
  <uri name="http://example.org/c/lib" uri="lib.ttl"/>
  <rewriteURI uriStartString="http://example.org/r/" rewritePrefix="r/"/>
</catalog>`;
    const form = page.getByRole('form', { name: 'Load an XML catalog' });
    await form.getByLabel('Catalog').setInputFiles({
      name: 'catalog-v001.xml',
      mimeType: 'application/xml',
      buffer: Buffer.from(catalog),
    });
    await form.getByLabel('Files').setInputFiles({
      name: 'lib.ttl',
      mimeType: 'text/turtle',
      buffer: Buffer.from(`<http://example.org/c/Lib> a <${OWL}Class> .`),
    });
    await form.getByRole('button', { name: 'Load catalog' }).click();
    await expect(page.getByRole('status').filter({ hasText: 'Loaded 1 file' })).toContainText(
      '1 skipped: rewriteURI http://example.org/r/',
    );
    await expect(mapTable(page)).toContainText('catalog');
    await expect(unresolved(page).locator('li')).toHaveCount(1);
    // the remaining import is fetched through the outbound policy
    await unresolved(page).getByRole('button', { name: 'Fetch into graph' }).click();
    await expect(page.getByRole('status').filter({ hasText: 'Fetched' })).toBeVisible();
    await expect(unresolved(page)).toHaveCount(0);
    await expect(mapTable(page)).toContainText('"mock-1"');
    await expect(mapTable(page).getByRole('button', { name: 'Refresh' })).toBeVisible();
    const commits = await (await request.get(`/$/commits/uni?branch=${branch}&limit=5`)).json();
    const messages = commits.commits.map((c: { message?: string }) => c.message ?? '');
    expect(messages[0]).toBe('Fetch the import http://imports.mock/units.ttl');
    expect(messages[1]).toBe('Load the XML catalog catalog-v001.xml');
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('excluding imported axioms reads the views without the imports', async ({ page, request }) => {
  const branch = 'imports-switch';
  await newBranch(request, branch);
  try {
    await seed(request, branch, 's');
    await openImports(page, branch);
    await expect(closure(page)).toBeVisible();
    const reads: string[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/$/ontology/uni?')) reads.push(r.url());
    });
    await page.getByRole('checkbox', { name: 'Imported axioms' }).uncheck();
    await expect(page).toHaveURL(/imports=0/);
    await expect(page.getByText('The views exclude imported axioms.')).toBeVisible();
    await expect.poll(() => reads.some((u) => u.includes('imports=false'))).toBe(true);
    await page.getByRole('checkbox', { name: 'Imported axioms' }).check();
    await expect(closure(page)).toBeVisible();
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a reader sees the closure and the map without the actions', async ({ page, request }) => {
  const branch = 'imports-reader';
  await newBranch(request, branch);
  try {
    await seed(request, branch, 'r');
    await page.route('**/$/whoami', (route) => route.fulfill({ json: whoami('reader', 'read') }));
    await page.setViewportSize({ width: 360, height: 760 });
    await openImports(page, branch);
    await expect(unresolved(page)).toContainText('http://example.org/r/missing');
    await expect(page.getByText('Import map')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Fetch into graph' })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Add entry' })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Load catalog' })).toHaveCount(0);
    await expectNoSidewaysScroll(page, 'the imports tab at phone width');
  } finally {
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});

test('a protected branch shows no actions and refuses a fetch', async ({ page, request }) => {
  const branch = 'imports-protected';
  await newBranch(request, branch);
  try {
    await update(
      request,
      branch,
      `INSERT DATA { <http://example.org/uni> <${OWL}imports> <http://imports.mock/units.ttl> }`,
    );
    const p = await request.patch(`/$/branches/uni/${branch}`, { data: { protected: true } });
    expect(p.ok(), await p.text()).toBe(true);
    await openImports(page, branch);
    await expect(unresolved(page)).toContainText('http://imports.mock/units.ttl');
    await expect(page.getByRole('button', { name: 'Fetch into graph' })).toHaveCount(0);
    const r = await request.post(`/$/ontology/uni/imports/fetch?branch=${branch}`, {
      data: { iri: 'http://imports.mock/units.ttl' },
    });
    expect(r.status()).toBe(403);
  } finally {
    await request.patch(`/$/branches/uni/${branch}`, { data: { protected: false } });
    await request.delete(`/$/branches/uni/${branch}?force=true`);
  }
});
