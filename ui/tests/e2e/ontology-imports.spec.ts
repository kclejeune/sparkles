// Import resolution in the ontology editor (ROADMAP item 624) against a real server. On the
// server without auth, an ontology in a named graph imports a library graph that imports it
// back, an import that no graph holds and one that an XML catalog's file supplies. The
// Imports tab shows the cycle and the unresolved import, a map entry resolves an import
// without a commit, and a catalog upload loads its file in one commit while main stays as it
// was. A fetch that the outbound policy refuses changes nothing, and a protected branch shows
// no actions and refuses a catalog. Under auth, a writer who is not an administrator sees the
// closure without the actions and is refused a map change.

import type { Page } from '@playwright/test';
import { IMPORTS_AUTH, PASSWORD, USER } from './data';
import { anonymous, expect, open, server } from './fixtures';

open.describe.configure({ mode: 'serial', timeout: 120_000 });

const DS = 'onto-imports-e2e';
const BRANCH = 'imports';
const GRAPH = 'http://example.org/app';
const LIB = 'http://example.org/lib';
const OWL = 'http://www.w3.org/2002/07/owl#';

const APP = `<${GRAPH}> a <${OWL}Ontology> ; <${OWL}imports> <${LIB}>, <http://example.org/missing>, <http://example.org/cat> .
<http://example.org/App> a <${OWL}Class> .`;
const LIBRARY = `<${LIB}> a <${OWL}Ontology> ; <${OWL}imports> <${GRAPH}> .
<http://example.org/Lib> a <${OWL}Class> .`;

async function openImports(page: Page, ds: string, branch: string | null) {
  const p = new URLSearchParams({ ds, graph: GRAPH, tab: 'imports' });
  if (branch) p.set('branch', branch);
  await page.goto(`/ui/workbench/ontology?${p}`);
  await expect(page.getByRole('region', { name: 'Imports' })).toBeVisible({ timeout: 30_000 });
}

const closure = (page: Page) => page.getByLabel('Imports closure');
const unresolved = (page: Page) => page.getByRole('list', { name: 'Unresolved imports' });
const mapTable = (page: Page) => page.getByRole('table', { name: 'Import map' });

open.beforeAll(async ({ request }) => {
  const r = await request.post('/$/datasets', { data: { dbName: DS, dbType: 'persistent' } });
  expect(r.ok(), await r.text()).toBe(true);
  for (const [g, data] of [
    [GRAPH, APP],
    [LIB, LIBRARY],
  ]) {
    const loaded = await request.post(`/${DS}/data?graph=${encodeURIComponent(g)}`, {
      headers: { 'Content-Type': 'text/turtle' },
      data,
    });
    expect(loaded.ok(), await loaded.text()).toBe(true);
  }
  const b = await request.post(`/$/branches/${DS}`, { data: { name: BRANCH } });
  expect(b.ok(), await b.text()).toBe(true);
});

open.afterAll(async ({ request }) => {
  await request.delete(`/$/datasets/${DS}`);
});

open(
  'the closure shows the cycle and the unresolved imports, and a map entry resolves one',
  async ({ page, request }) => {
    await openImports(page, DS, BRANCH);
    await expect(closure(page).getByText('cycle')).toHaveCount(2);
    await expect(unresolved(page).locator('li')).toHaveCount(2);
    await expect(unresolved(page)).toContainText('http://example.org/missing');
    const head = (
      await (await request.get(`/$/ontology/${DS}?graph=${GRAPH}&branch=${BRANCH}`)).json()
    ).commit;
    await page.getByRole('textbox', { name: 'Import IRI' }).fill('http://example.org/missing');
    await page.getByRole('textbox', { name: 'Graph' }).fill(LIB);
    await page.getByRole('button', { name: 'Add entry' }).click();
    await expect(mapTable(page)).toContainText('manual');
    await expect(unresolved(page).locator('li')).toHaveCount(1);
    await expect(closure(page)).toContainText('by the import map');
    // the map changes what a read resolves without a commit
    const after = await (
      await request.get(`/$/ontology/${DS}?graph=${GRAPH}&branch=${BRANCH}`)
    ).json();
    expect(after.commit).toBe(head);
    await mapTable(page)
      .getByRole('button', { name: 'Remove the entry for http://example.org/missing' })
      .click();
    await expect(unresolved(page).locator('li')).toHaveCount(2);
  },
);

open(
  'a catalog loads its file on the branch, and main stays as it was',
  async ({ page, request }) => {
    await openImports(page, DS, BRANCH);
    const form = page.getByRole('form', { name: 'Load an XML catalog' });
    await form.getByLabel('Catalog').setInputFiles({
      name: 'catalog-v001.xml',
      mimeType: 'application/xml',
      buffer: Buffer.from(`<?xml version="1.0"?>
<catalog prefer="public" xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog">
  <group prefer="public"><uri name="http://example.org/cat" uri="cat.ttl"/></group>
  <nextCatalog catalog="other.xml"/>
</catalog>`),
    });
    await form.getByLabel('Files').setInputFiles({
      name: 'cat.ttl',
      mimeType: 'text/turtle',
      buffer: Buffer.from(`<http://example.org/Cat> a <${OWL}Class> .`),
    });
    await form.getByRole('button', { name: 'Load catalog' }).click();
    await expect(page.getByRole('status').filter({ hasText: 'Loaded 1 file' })).toContainText(
      'nextCatalog other.xml',
    );
    await expect(mapTable(page)).toContainText('catalog');
    await expect(unresolved(page).locator('li')).toHaveCount(1);
    const onBranch = await request.get(`/$/ontology/${DS}/imports`);
    expect((await onBranch.json()).entries[0].digest).toMatch(/^sha256:/);
    const commits = await (await request.get(`/$/commits/${DS}?branch=${BRANCH}&limit=3`)).json();
    expect(commits.commits[0].message).toBe('Load the XML catalog catalog-v001.xml');
    // main has no copy of the catalog's file, so its import stays unresolved there
    const main = await (await request.get(`/$/ontology/${DS}?graph=${GRAPH}`)).json();
    const cat = main.closure.find((e: { iri: string }) => e.iri === 'http://example.org/cat');
    expect(cat.graphs).toEqual([]);
  },
);

open('the outbound policy refuses a fetch and nothing changes', async ({ request }) => {
  const head = (
    await (await request.get(`/$/ontology/${DS}?graph=${GRAPH}&branch=${BRANCH}`)).json()
  ).commit;
  const r = await request.post(`/$/ontology/${DS}/imports/fetch?branch=${BRANCH}`, {
    data: { iri: 'http://example.org/missing', location: 'http://127.0.0.1:9/missing.ttl' },
  });
  expect(r.status(), await r.text()).toBe(403);
  const after = await (
    await request.get(`/$/ontology/${DS}?graph=${GRAPH}&branch=${BRANCH}`)
  ).json();
  expect(after.commit).toBe(head);
});

open('a protected branch shows no actions and refuses a catalog', async ({ page, request }) => {
  const p = await request.patch(`/$/branches/${DS}/${BRANCH}`, { data: { protected: true } });
  expect(p.ok(), await p.text()).toBe(true);
  try {
    await openImports(page, DS, BRANCH);
    await expect(unresolved(page)).toContainText('http://example.org/missing');
    await expect(page.getByRole('button', { name: 'Load catalog' })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Add entry' })).toHaveCount(0);
    const r = await request.post(`/$/ontology/${DS}/imports/catalog?branch=${BRANCH}`, {
      multipart: {
        catalog: {
          name: 'c.xml',
          mimeType: 'application/xml',
          buffer: Buffer.from('<catalog><uri name="http://example.org/z" uri="urn:z"/></catalog>'),
        },
      },
    });
    expect(r.status(), await r.text()).toBe(403);
  } finally {
    await request.patch(`/$/branches/${DS}/${BRANCH}`, { data: { protected: false } });
  }
});

const admin = () => ({ Authorization: `Bearer ${server.token}` });

anonymous.describe('under auth', () => {
  anonymous.describe.configure({ mode: 'serial', timeout: 120_000 });

  anonymous.beforeAll(async ({ request }) => {
    const r = await request.post('/$/datasets', {
      headers: admin(),
      data: { dbName: IMPORTS_AUTH, dbType: 'mem' },
    });
    expect(r.ok(), await r.text()).toBe(true);
    for (const [g, data] of [
      [GRAPH, APP],
      [LIB, LIBRARY],
    ]) {
      const loaded = await request.post(`/${IMPORTS_AUTH}/data?graph=${encodeURIComponent(g)}`, {
        headers: { ...admin(), 'Content-Type': 'text/turtle' },
        data,
      });
      expect(loaded.ok(), await loaded.text()).toBe(true);
    }
  });

  anonymous(
    'a writer who is not an administrator sees the closure without the actions',
    async ({ page }) => {
      const login = await page.request.post('/$/auth/login', {
        data: { user: USER, password: PASSWORD },
      });
      expect(login.ok()).toBe(true);
      await openImports(page, IMPORTS_AUTH, null);
      await expect(closure(page).getByText('cycle')).toHaveCount(2);
      await expect(page.getByText('only administrators change the map')).toBeVisible();
      await expect(page.getByRole('button', { name: 'Add entry' })).toHaveCount(0);
      const w = await page.request.patch(`/$/ontology/${IMPORTS_AUTH}/imports`, {
        data: { set: { 'http://example.org/missing': LIB } },
      });
      expect(w.status()).toBe(403);
    },
  );
});
