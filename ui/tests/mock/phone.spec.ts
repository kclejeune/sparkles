// Every page, tab, dialog and menu at a phone's width (320 CSS pixels): the page never
// scrolls sideways, only the wide content inside it does. Read-only against the mock: the
// dialogs are opened and left without saving anything.

import { expect, test, type Page } from '@playwright/test';
import { expectNoSidewaysScroll, phone, selectAll } from '../phone';

test.use(phone);
test.describe.configure({ timeout: 90_000 });

async function settle(page: Page) {
  await page.waitForLoadState('networkidle');
  await expect(page.locator('main .spinner')).toHaveCount(0, { timeout: 15_000 });
}

async function visit(page: Page, url: string) {
  await page.goto(url);
  await settle(page);
  await expectNoSidewaysScroll(page, url);
}

/** Clicks `name` (a button, or a tab), then checks the page with what it opened. */
async function open(page: Page, name: string | RegExp, role: 'button' | 'tab' = 'button') {
  await page.getByRole(role, { name }).first().click();
  await settle(page);
  await expectNoSidewaysScroll(page, `${page.url()} after ${name}`);
}

async function run(page: Page, ds: string, query: string) {
  await page.addInitScript((d) => localStorage.setItem('sparkles.dataset', d), ds);
  await page.goto('/ui/query');
  await page.locator('.cm-content').click();
  await selectAll(page);
  await page.keyboard.insertText(query);
  await page.getByRole('button', { name: /^Run\b/ }).click();
  const results = page.getByRole('region', { name: 'Results' });
  await expect(results.getByRole('tab', { name: /Table/ })).toBeVisible();
  await expectNoSidewaysScroll(page, 'the query page');
  return results;
}

test('the query page, each result view and its menus', async ({ page }) => {
  const results = await run(page, 'foaf', 'SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 50');
  for (const view of [/Graph/, /Plan/, /Raw/, /Table/]) {
    await results.getByRole('tab', { name: view }).click();
    await expectNoSidewaysScroll(page, `the query page's ${view.source} view`);
  }
  await open(page, /^Explain\b/);
  await open(page, 'Examples');
  await page.getByRole('button', { name: 'Examples' }).click();
  await open(page, 'Saved');
  await page.getByRole('menuitem', { name: /Save this query/ }).click();
  await expectNoSidewaysScroll(page, 'the save dialog');
  await page.keyboard.press('Escape');
  await open(page, 'Format options');
  await page.locator('.ds-button').click();
  await expect(page.getByRole('listbox', { name: 'Datasets' })).toBeVisible();
  await expectNoSidewaysScroll(page, 'the dataset switcher');
});

test('the Map view', async ({ page }) => {
  const results = await run(
    page,
    'places',
    `PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
SELECT ?f ?label ?w WHERE {
  ?f rdfs:label ?label ; geo:hasDefaultGeometry|geo:hasGeometry ?g .
  ?g geo:asWKT|geo:asGeoJSON ?w .
}`,
  );
  await results.getByRole('tab', { name: 'Map' }).click();
  await expect(page.getByRole('region', { name: 'Result map' })).toHaveAttribute(
    'data-state',
    'ready',
  );
  await expectNoSidewaysScroll(page, 'the Map view');
});

test('the explorer: graph, schema and text search', async ({ page }) => {
  await visit(
    page,
    `/ui/explore?ds=foaf&iri=${encodeURIComponent('http://example.org/people/alice')}`,
  );
  await visit(page, '/ui/explore?ds=foaf&tab=schema');
  await open(page, 'Draft shapes');
  await page
    .getByRole('dialog')
    .getByRole('button', { name: /^Draft$/ })
    .click();
  await expect(page.getByLabel('Draft text')).toContainText('sh:NodeShape');
  await expectNoSidewaysScroll(page, 'the drafted shapes');
  await page.keyboard.press('Escape');
  await visit(page, '/ui/explore?ds=places&iri=http%3A%2F%2Fexample.org%2Fplaces%2Fparis');
  await page.goto('/ui/explore?ds=foaf&tab=search');
  await page.getByLabel('Full-text query').fill('a');
  await open(page, /^Search$/);
});

test('the dataset list and a dataset with its panels and dialogs', async ({ page }) => {
  await visit(page, '/ui/datasets');
  await open(page, 'New dataset');
  await page.keyboard.press('Escape');

  // (the mock validates SHACL only; tests/e2e/phone.spec.ts has the ShEx tab)
  await visit(page, '/ui/datasets/foaf');
  for (const dialog of [
    'Clone',
    /^Delete$/,
    /^Restore…$/,
    'Back up now',
    /^(Configure|Enable)…$/,
    'New index…',
    /^Edit…$/,
  ]) {
    await open(page, dialog);
    await page.keyboard.press('Escape');
    await expect(page.locator('dialog[open]')).toHaveCount(0);
  }
  // the spatial index panel
  await visit(page, '/ui/datasets/places');
});

test('a branch: the selector, the Branches panel and its dialogs', async ({ page, request }) => {
  // the one write of these tests: a branch to show (it may exist from an earlier run)
  await request.post('/$/branches/foaf', { data: { name: 'phone-check', note: 'a long note' } });
  await visit(page, '/ui/datasets/foaf?branch=phone-check');
  await expect(page.getByRole('combobox', { name: 'Branch' })).toHaveValue('phone-check');
  await open(page, 'New branch');
  for (const dialog of ['Merge phone-check', 'Delete branch phone-check']) {
    await open(page, dialog);
    await page.keyboard.press('Escape');
    await expect(page.locator('dialog[open]')).toHaveCount(0);
  }
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'foaf'));
  await visit(page, '/ui/query');
  await expect(page.getByRole('combobox', { name: 'Branch' })).toBeVisible();
});

test('Similar: from an entity and from a pasted vector', async ({ page }) => {
  await visit(
    page,
    `/ui/similar?ds=foaf&index=embedding&iri=${encodeURIComponent('http://example.org/resource/Ada_Lovelace')}`,
  );
  await expect(page.locator('tbody tr')).toHaveCount(10);
  await expectNoSidewaysScroll(page, 'the Similar results');
  await page.getByRole('radio', { name: 'Vector' }).click();
  await page
    .getByLabel(/^Vector/)
    .fill(`[${Array.from({ length: 8 }, () => '0.123456').join(', ')}]`);
  await open(page, /^Search$/);
});

test('backups: every tab, the drawer and the dialogs', async ({ page }) => {
  await visit(page, '/ui/backups');
  await open(page, 'foaf-before-reload');
  await page.keyboard.press('Escape');
  await open(page, /Repositories/, 'tab');
  await open(page, 'Add repository');
  await page.keyboard.press('Escape');
  await open(page, /Policies/, 'tab');
  await open(page, 'New policy');
  await page.keyboard.press('Escape');
  await open(page, /Activity/, 'tab');
});

test('the server, tokens, sign-in and CLI pages', async ({ page }) => {
  for (const url of ['/ui/', '/ui/server', '/ui/tokens', '/ui/login', '/ui/cli/device'])
    await visit(page, url);
});
