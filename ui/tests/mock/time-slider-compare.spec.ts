// The time slider's compare modes (docs/specs/U01-time-slider.md, Phase 3): marks on the
// Map view, a comparison with the head of another branch, and the comparison the server
// makes when the browser cannot. Each test group works on a dataset of its own.
//
//   geochrono  commit 1 draws places A and C, commit 2 adds B and removes C
//   forked     main and dev share commit 1 (ex:root ex:link ex:base); main adds ex:m, dev ex:d
//   bulk       commit 1 holds 1,100 rows, commit 2 adds three more

import { expect, test, type APIRequestContext, type Page } from '@playwright/test';

test.describe.configure({ mode: 'serial', timeout: 90_000 });

const PREFIXES = `PREFIX ex: <http://example.org/>
PREFIX geo: <http://www.opengis.net/ont/geosparql#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>`;

async function update(request: APIRequestContext, ds: string, sparql: string, message: string) {
  const r = await request.post(`/${ds}/update`, {
    headers: { 'Content-Type': 'application/sparql-update', 'Sparkles-Commit-Message': message },
    data: `${PREFIXES} ${sparql}`,
  });
  expect(r.ok(), `update ${ds}: ${message}`).toBe(true);
}

async function create(request: APIRequestContext, name: string): Promise<boolean> {
  const r = await request.post('/$/datasets', { data: { dbName: name, dbType: 'persistent' } });
  if (r.status() === 409) return false;
  expect(r.ok()).toBe(true);
  return true;
}

const place = (id: string, label: string, wkt: string) =>
  `ex:${id} rdfs:label "${label}" ; geo:hasGeometry ex:g${id} .
   ex:g${id} geo:asWKT "${wkt}"^^geo:wktLiteral .`;

test.beforeAll(async ({ request }) => {
  if (await create(request, 'geochrono')) {
    await update(
      request,
      'geochrono',
      `INSERT DATA { ${place('a', 'Alpha', 'POINT(2.29 48.85)')} ${place('c', 'Gamma', 'POINT(-0.12 51.5)')} }`,
      'Alpha and Gamma',
    );
    await update(
      request,
      'geochrono',
      `INSERT DATA { ${place('b', 'Beta', 'POINT(13.4 52.5)')} } ;
       DELETE DATA { ex:gc geo:asWKT "POINT(-0.12 51.5)"^^geo:wktLiteral }`,
      'Beta in, Gamma out',
    );
  }
  if (await create(request, 'forked')) {
    await update(request, 'forked', 'INSERT DATA { ex:root ex:link ex:base }', 'Root');
    const b = await request.post('/$/branches/forked', { data: { name: 'dev' } });
    expect(b.status()).toBe(201);
    await update(request, 'forked', 'INSERT DATA { ex:root ex:link ex:m }', 'Main adds m');
    await update(request, 'forked@dev', 'INSERT DATA { ex:root ex:link ex:d }', 'Dev adds d');
  }
  if (await create(request, 'bulk')) {
    const rows = Array.from({ length: 1100 }, (_, i) => `ex:n${i} ex:v ${i} .`).join(' ');
    await update(request, 'bulk', `INSERT DATA { ${rows} }`, 'Load');
    await update(
      request,
      'bulk',
      'INSERT DATA { ex:x1 ex:v 5000 . ex:x2 ex:v 5001 . ex:x3 ex:v 5002 }',
      'Three more',
    );
  }
});

const slider = (page: Page) => page.getByRole('region', { name: 'History time slider' });
const right = (page: Page) => slider(page).getByRole('slider', { name: /^(Commit|Compared to)$/ });
const grid = (page: Page) => page.getByRole('grid');
const run = (page: Page) => page.getByRole('button', { name: /^Run/ }).click();

async function prepare(page: Page, ds: string, query: string, limit = 10_000) {
  await page.addInitScript(
    ([ds, query, limit]) => {
      localStorage.setItem('sparkles.dataset', ds as string);
      localStorage.setItem(
        'sparkles.queryTabs',
        JSON.stringify({
          tabs: [{ id: 't1', title: 'Query 1', query }],
          active: 't1',
          limit,
        }),
      );
    },
    [ds, query, limit],
  );
}

test('the Map view draws the added and removed places of a comparison', async ({ page }) => {
  await prepare(
    page,
    'geochrono',
    `${PREFIXES}
SELECT ?label ?w WHERE { ?f rdfs:label ?label ; geo:hasGeometry ?g . ?g geo:asWKT ?w } ORDER BY ?label`,
  );
  await page.goto('/ui/query');
  await run(page);
  await expect(grid(page)).toContainText('Beta');
  await page.getByRole('button', { name: 'History', exact: true }).click();
  await right(page).focus();
  await page.keyboard.press('c');
  const results = page.getByRole('region', { name: 'Results' });
  await expect(
    results.getByRole('grid').getByRole('row').filter({ hasText: 'Gamma' }),
  ).toBeVisible();
  await results.getByRole('tab', { name: 'Map' }).click();
  const map = page.getByRole('region', { name: 'Result map' });
  await expect(map).toHaveAttribute('data-state', 'ready');
  // Alpha is kept, Beta is new and Gamma is gone
  await expect(map).toHaveAttribute('data-features', '3');
  await expect(map).toHaveAttribute('data-added', '1');
  await expect(map).toHaveAttribute('data-removed', '1');
  const rows = page
    .getByRole('region', { name: 'Result map' })
    .locator('..')
    .getByLabel('Rows on the map');
  await expect(rows.getByRole('button', { name: /Beta/ }).getByLabel('added')).toHaveText('+');
  await expect(rows.getByRole('button', { name: /Gamma/ }).getByLabel('removed')).toHaveText('−');
  await expect(rows.getByRole('button', { name: /Alpha/ }).getByLabel('added')).toHaveCount(0);
});

test('a query is compared with the head of another branch', async ({ page }) => {
  await prepare(page, 'forked', `${PREFIXES}\nSELECT ?o WHERE { ex:root ex:link ?o } ORDER BY ?o`);
  await page.goto('/ui/query');
  await run(page);
  await expect(grid(page)).toContainText('/m');
  await page.getByRole('button', { name: 'History', exact: true }).click();
  await right(page).focus();
  await page.keyboard.press('c');
  await slider(page).getByRole('combobox', { name: 'Compare with' }).selectOption('dev');
  // dev has ex:d where main has ex:m
  const added = grid(page).getByRole('row').filter({ hasText: '/m' });
  await expect(added.getByRole('gridcell', { name: 'added' })).toHaveText('+');
  const removed = grid(page).getByRole('row').filter({ hasText: '/d' });
  await expect(removed.getByRole('gridcell', { name: 'removed' })).toHaveText('−');
  const kept = grid(page).getByRole('row').filter({ hasText: '/base' });
  await expect(kept.getByRole('gridcell', { name: /added|removed/ })).toHaveCount(0);
  await expect(page.getByText('against the head of dev')).toBeVisible();
  expect(new URL(page.url()).searchParams.get('against')).toBe('dev');
  // the link restores the comparison
  await page.reload();
  await expect(
    grid(page)
      .getByRole('row')
      .filter({ hasText: '/d' })
      .getByRole('gridcell', { name: 'removed' }),
  ).toBeVisible();
});

test('the Explore graph is compared with the head of another branch', async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'forked'));
  await page.goto(`/ui/explore?ds=forked&iri=${encodeURIComponent('http://example.org/root')}`);
  const drawn = async () => {
    const raw = await page.locator('.wrap[data-graph]').getAttribute('data-graph');
    return (raw ? JSON.parse(raw) : { nodes: [] }) as { nodes: [string, string][] };
  };
  await expect.poll(async () => (await drawn()).nodes.map(([l]) => l).join(' ')).toMatch(/base/);
  await page.getByRole('button', { name: 'Graph history' }).click();
  const bar = page.getByRole('region', { name: 'Graph history time slider' });
  await bar.getByRole('slider', { name: /^(Commit|Compared to)$/ }).focus();
  await page.keyboard.press('c');
  await bar.getByRole('combobox', { name: 'Compare with' }).selectOption('dev');
  const changeOf = async (re: RegExp) => (await drawn()).nodes.find(([l]) => re.test(l))?.[1];
  await expect.poll(() => changeOf(/\bm\b/)).toBe('added');
  await expect.poll(() => changeOf(/\bd\b/)).toBe('removed');
  expect(new URL(page.url()).searchParams.get('against')).toBe('dev');
});

test('Compare on the server lists the rows the browser did not read', async ({ page }) => {
  await prepare(page, 'bulk', `${PREFIXES}\nSELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?v`, 1_000);
  await page.goto('/ui/query');
  await run(page);
  await expect(grid(page)).toBeVisible();
  await page.getByRole('button', { name: 'History', exact: true }).click();
  await right(page).focus();
  await page.keyboard.press('c');
  const button = page.getByRole('button', { name: 'Compare on the server' });
  await expect(button).toBeVisible();
  const asked = page.waitForRequest((r) => r.url().includes('/bulk/sparql/diff'));
  await button.click();
  const req = await asked;
  expect(req.method()).toBe('POST');
  expect(new URL(req.url()).searchParams.get('from')).toBe('commit:1');
  // only the three differing rows are left, all added
  await expect(grid(page).getByRole('row')).toHaveCount(4);
  await expect(grid(page)).toContainText('5002');
  await expect(
    grid(page)
      .getByRole('row')
      .filter({ hasText: '5000' })
      .getByRole('gridcell', { name: 'added' }),
  ).toHaveText('+');
  await expect(
    page.getByRole('status').filter({ hasText: 'Compared on the server' }),
  ).toBeVisible();
});

test('a read that cut its rows compares counts only, with no marks', async ({ page }) => {
  await prepare(page, 'bulk', `${PREFIXES}\nSELECT ?s ?v WHERE { ?s ex:v ?v } ORDER BY ?v`, 1_000);
  await page.goto('/ui/query');
  await run(page);
  await expect(grid(page)).toBeVisible();
  await page.getByRole('button', { name: 'History', exact: true }).click();
  await right(page).focus();
  await page.keyboard.press('c');
  await expect(page.getByText('The read cut the rows')).toBeVisible();
  await expect(page.getByText('so only the counts are compared')).toBeVisible();
  // a prefix of the rows says nothing about the rest, so no row carries a mark
  await expect(grid(page).getByRole('gridcell', { name: /^(added|removed)$/ })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Compare on the server' })).toBeVisible();
});

test('a schema link that compares with a branch reopens the slider in compare mode', async ({
  page,
}) => {
  await page.addInitScript(() => localStorage.setItem('sparkles.dataset', 'forked'));
  await page.goto('/ui/explore?ds=forked&tab=schema&against=dev');
  const bar = page.getByRole('region', { name: 'Schema history time slider' });
  await expect(bar).toBeVisible();
  await expect(bar.getByRole('combobox', { name: 'Compare with' })).toHaveValue('dev');
  await page.reload();
  await expect(bar.getByRole('combobox', { name: 'Compare with' })).toHaveValue('dev');
});

test('a history link without a branch reads main after one that named a branch', async ({
  page,
}) => {
  await prepare(page, 'forked', `${PREFIXES}\nSELECT ?o WHERE { ex:root ex:link ?o } ORDER BY ?o`);
  const reads: string[] = [];
  page.on('request', (r) => {
    const path = new URL(r.url()).pathname;
    if (r.method() === 'POST' && /\/forked(@dev)?\/sparql$/.test(path)) reads.push(path);
  });
  await page.goto('/ui/query?ds=forked&at=commit%3A1&branch=dev');
  await expect(grid(page)).toContainText('/base');
  expect(reads.at(-1)).toBe('/forked@dev/sparql');
  // leave the page, then follow a link without `branch` inside the app
  await page.getByRole('link', { name: 'Explore' }).first().click();
  await expect(page).toHaveURL(/\/ui\/explore/);
  reads.length = 0;
  await page.evaluate(() => {
    const a = document.createElement('a');
    a.href = '/ui/query?ds=forked&at=commit%3A1';
    a.id = 'history-link';
    document.body.append(a);
    a.click();
  });
  await expect(grid(page)).toContainText('/base');
  expect(reads.at(-1)).toBe('/forked/sparql');
});
